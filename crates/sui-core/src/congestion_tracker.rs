use moka::ops::compute::Op;
use moka::sync::Cache;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use sui_types::base_types::ObjectID;
use sui_types::effects::{InputSharedObject, TransactionEffects, TransactionEffectsAPI};
use sui_types::execution_status::CongestedObjects;
use sui_types::messages_checkpoint::{CheckpointTimestamp, VerifiedCheckpoint};
use sui_types::transaction::{TransactionData, TransactionDataAPI};

use crate::execution_cache::TransactionCacheRead;

#[derive(Clone, Copy, Debug)]
pub struct CongestionInfo {
    pub last_cancellation_time: CheckpointTimestamp,

    pub highest_cancelled_gas_price: u64,

    pub last_success_time: Option<CheckpointTimestamp>,
    pub lowest_executed_gas_price: Option<u64>,
}

impl CongestionInfo {
    /// Update the congestion info with the latest congestion info from a new checkpoint
    fn update_for_new_checkpoint(&mut self, new: &CongestionInfo) {
        // If there are more recent cancellations, we need to know the latest highest
        // cancelled price
        if new.last_cancellation_time > self.last_cancellation_time {
            self.last_cancellation_time = new.last_cancellation_time;
            self.highest_cancelled_gas_price = new.highest_cancelled_gas_price;
        }
        // If there are more recent successful transactions, we need to know the latest lowest
        // executed price
        if new.last_success_time > self.last_success_time {
            self.last_success_time = new.last_success_time;
            self.lowest_executed_gas_price = new.lowest_executed_gas_price;
        }
    }

    fn update_for_cancellation(&mut self, now: CheckpointTimestamp, gas_price: u64) {
        self.last_cancellation_time = now;
        self.highest_cancelled_gas_price =
            std::cmp::max(self.highest_cancelled_gas_price, gas_price);
    }

    fn update_for_success(&mut self, now: CheckpointTimestamp, gas_price: u64) {
        self.last_success_time = Some(now);
        self.lowest_executed_gas_price = Some(match self.lowest_executed_gas_price {
            Some(current_min) => std::cmp::min(current_min, gas_price),
            None => gas_price,
        });
    }
}

pub struct CongestionTracker {
    pub congestion_clearing_prices: Cache<ObjectID, CongestionInfo>,
}

impl CongestionTracker {
    pub fn new() -> Self {
        Self {
            congestion_clearing_prices: Cache::new(10_000),
        }
    }

    pub fn process_checkpoint_effects(
        &self,
        transaction_cache_reader: &dyn TransactionCacheRead,
        checkpoint: &VerifiedCheckpoint,
        effects: &[TransactionEffects],
    ) {
        let mut congestion_events = Vec::with_capacity(effects.len());
        let mut cleared_events = Vec::with_capacity(effects.len());

        for effect in effects {
            let gas_price = transaction_cache_reader
                .get_transaction_block(effect.transaction_digest())
                .unwrap()
                .transaction_data()
                .gas_price();
            if let Some(CongestedObjects(congested_objects)) =
                effect.status().get_congested_objects()
            {
                congestion_events.push((gas_price, congested_objects.clone()));
            } else {
                cleared_events.push((
                    gas_price,
                    effect
                        .input_shared_objects()
                        .into_iter()
                        .filter_map(|object| match object {
                            InputSharedObject::Mutate((id, _, _)) => Some(id),
                            InputSharedObject::Cancelled(_, _)
                            | InputSharedObject::ReadOnly(_)
                            | InputSharedObject::ReadDeleted(_, _)
                            | InputSharedObject::MutateDeleted(_, _) => None,
                        })
                        .collect::<Vec<_>>(),
                ));
            }
        }

        let congestion_info_map =
            self.process_events(checkpoint.timestamp_ms, &congestion_events, &cleared_events);
        self.process_per_checkpoint_events(congestion_info_map);
    }

    fn process_events(
        &self,
        now: CheckpointTimestamp,
        congestion_events: &[(u64, Vec<ObjectID>)],
        cleared_events: &[(u64, Vec<ObjectID>)],
    ) -> HashMap<ObjectID, CongestionInfo> {
        let mut congestion_info_map: HashMap<ObjectID, CongestionInfo> = HashMap::new();

        for (gas_price, objects) in congestion_events {
            for object in objects {
                match congestion_info_map.entry(*object) {
                    Entry::Occupied(entry) => {
                        entry.into_mut().update_for_cancellation(now, *gas_price);
                    }
                    Entry::Vacant(entry) => {
                        let info = CongestionInfo {
                            last_cancellation_time: now,
                            highest_cancelled_gas_price: *gas_price,
                            last_success_time: None,
                            lowest_executed_gas_price: None,
                        };

                        entry.insert(info);
                    }
                }
            }
        }

        for (gas_price, objects) in cleared_events {
            for object in objects {
                // We only record clearing prices if the object has observed cancellations recently
                match congestion_info_map.entry(*object) {
                    Entry::Occupied(entry) => {
                        entry.into_mut().update_for_success(now, *gas_price);
                    }
                    Entry::Vacant(entry) => {
                        if let Some(prev) = self.get_congestion_info(*object) {
                            let info = CongestionInfo {
                                last_cancellation_time: prev.last_cancellation_time,
                                highest_cancelled_gas_price: prev.highest_cancelled_gas_price,
                                last_success_time: Some(now),
                                lowest_executed_gas_price: Some(*gas_price),
                            };
                            entry.insert(info);
                        }
                    }
                }
            }
        }

        congestion_info_map
    }

    fn process_per_checkpoint_events(
        &self,
        congestion_info_map: HashMap<ObjectID, CongestionInfo>,
    ) {
        for (object_id, info) in congestion_info_map {
            self.congestion_clearing_prices
                .entry(object_id)
                .and_compute_with(|maybe_entry| {
                    if let Some(e) = maybe_entry {
                        let mut e = e.into_value();
                        e.update_for_new_checkpoint(&info);
                        Op::Put(e)
                    } else {
                        Op::Nop
                    }
                });
        }
    }

    fn get_congestion_info(&self, object_id: ObjectID) -> Option<CongestionInfo> {
        self.congestion_clearing_prices.get(&object_id)
    }

    /// For all the mutable shared inputs, get the highest minimum clearing price (if any exists)
    /// and the lowest maximum cancelled price.
    pub fn get_suggested_gas_prices(&self, transaction: &TransactionData) -> Option<u64> {
        let mut clearing_price = None;
        for object_id in transaction
            .shared_input_objects()
            .into_iter()
            .filter(|id| id.mutable)
            .map(|id| id.id)
        {
            if let Some(info) = self.get_congestion_info(object_id) {
                let clearing_price_for_object = match info
                    .last_success_time
                    .cmp(&Some(info.last_cancellation_time))
                {
                    std::cmp::Ordering::Greater => {
                        // there were no cancellations in the most recent checkpoint,
                        // so the object is probably not congested any more
                        None
                    }
                    std::cmp::Ordering::Less => {
                        // there were no successes in the most recent checkpoint. This should be a rare case,
                        // but we know we will have to bid at least as much as the highest cancelled price.
                        Some(info.highest_cancelled_gas_price)
                    }
                    std::cmp::Ordering::Equal => {
                        // there were both successes and cancellations.
                        info.lowest_executed_gas_price
                    }
                };
                clearing_price = std::cmp::max(clearing_price, clearing_price_for_object);
            }
        }
        clearing_price
    }
}

impl Default for CongestionTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn test_congestion_tracker() {}
}
