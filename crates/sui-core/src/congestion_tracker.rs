use moka::ops::compute::Op;
use moka::sync::Cache;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::time::Instant;
use sui_types::base_types::ObjectID;
use sui_types::effects::{InputSharedObject, TransactionEffects, TransactionEffectsAPI};
use sui_types::executable_transaction::VerifiedExecutableTransaction;
use sui_types::execution_status::CongestedObjects;
use sui_types::messages_checkpoint::VerifiedCheckpoint;
use sui_types::transaction::{TransactionData, TransactionDataAPI};

use crate::execution_cache::TransactionCacheRead;

#[derive(Clone, Copy, Debug)]
pub struct CongestionInfo {
    pub last_cancellation_time: Instant,

    pub highest_cancelled_gas_price: u64,

    pub last_success_time: Option<Instant>,
    pub lowest_executed_gas_price: Option<u64>,
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
        let now = Instant::now();

        let mut congestion_info_map: HashMap<ObjectID, CongestionInfo> = HashMap::new();
        let mut cleared_effects = Vec::with_capacity(effects.len());

        for effect in effects {
            let gas_price = transaction_cache_reader
                .get_transaction_block(effect.transaction_digest())
                .unwrap()
                .transaction_data()
                .gas_price();
            if let Some(CongestedObjects(congested_objects)) =
                effect.status().get_congested_objects()
            {
                for object in congested_objects {
                    match congestion_info_map.entry(*object) {
                        Entry::Occupied(entry) => {
                            entry.into_mut().update_for_cancellation(now, gas_price);
                        }
                        Entry::Vacant(entry) => {
                            let info = CongestionInfo {
                                last_cancellation_time: now,
                                highest_cancelled_gas_price: gas_price,
                                last_success_time: None,
                                lowest_executed_gas_price: None,
                            };

                            entry.insert(info);
                        }
                    }
                }
            } else {
                cleared_effects.push((effect, gas_price));
            }
        }

        for (effect, gas_price) in cleared_effects {
            for object in effect.input_shared_objects() {
                // We only record clearing prices if the object has observed cancellations recently
                match object {
                    InputSharedObject::Mutate((id, _, _)) => match congestion_info_map.entry(id) {
                        Entry::Occupied(entry) => {
                            entry.into_mut().update_for_success(now, gas_price);
                        }
                        Entry::Vacant(entry) => {
                            if let Some(prev) = self.get_congestion_info(id) {
                                let info = CongestionInfo {
                                    last_cancellation_time: prev.last_cancellation_time,
                                    highest_cancelled_gas_price: prev.highest_cancelled_gas_price,
                                    last_success_time: Some(now),
                                    lowest_executed_gas_price: Some(gas_price),
                                };
                                entry.insert(info);
                            }
                        }
                    },

                    InputSharedObject::Cancelled(_, _) => {}
                    InputSharedObject::ReadOnly(_) => {}
                    InputSharedObject::ReadDeleted(_, _) => {}
                    InputSharedObject::MutateDeleted(_, _) => {}
                }
            }
        }
    }

    pub fn update_congestion_info(
        &self,
        certificate: &VerifiedExecutableTransaction,
        effects: &TransactionEffects,
    ) {
        let tx = &certificate.data().intent_message().value;
        let gas_price = tx.gas_price();
        let now = Instant::now();

        if let Some(CongestedObjects(congested_objects)) = effects.status().get_congested_objects()
        {
            self.update_congestion_info_for_cancelled_objects(
                now,
                congested_objects.iter(),
                gas_price,
            );
        } else {
            self.update_congestion_info_for_successful_transaction(
                now,
                tx.shared_input_objects()
                    .iter()
                    .filter(|id| id.mutable)
                    .map(|id| &id.id),
                gas_price,
            );
        }
    }

    pub fn get_congestion_info(&self, object_id: ObjectID) -> Option<CongestionInfo> {
        self.congestion_clearing_prices.get(&object_id)
    }

    /// For all the mutable shared inputs, get the highest minimum clearing price (if any exists)
    /// and the lowest maximum cancelled price.
    pub fn get_suggested_gas_prices(&self, transaction: &TransactionData) -> u64 {
        let mut recommended = transaction.gas_price();
        for object_id in transaction
            .shared_input_objects()
            .into_iter()
            .filter(|id| id.mutable)
            .map(|id| id.id)
        {
            if let Some(info) = self.get_congestion_info(object_id) {
                // if the cleared price for this object is higher than the cancelled price,
                if let Some(cleared) = info.lowest_executed_gas_price {
                    // when there is a cleared price there are four cases
                    // clear > cancelled, cleared newer than cancelled
                    // clear > cancelled, cleared older than cancelled
                    // clear < cancelled, cleared newer than cancelled
                    // clear < cancelled, cleared older than cancelled
                    if cleared > recommended {
                        recommended = cleared;
                    }
                }

                match (
                    info.lowest_executed_gas_price,
                    info.highest_cancelled_gas_price,
                ) {
                    (Some(cleared), Some(cancelled)) => {
                        recommended = std::cmp::max(recommended, std::cmp::min(cleared, cancelled));
                    }
                    (Some(cleared), None) => {
                        recommended = std::cmp::max(recommended, cleared);
                    }
                }
            }
        }
        match (highest_min_cleared, highest_max_cancelled) {
            // The highest cancelled can be
            (Some(cleared), Some(cancelled)) => std::cmp::max(cleared, cancelled),
            (Some(cleared), None) => cleared,
            (None, Some(cancelled)) => cancelled,
            (None, None) => 0,
        }
    }

    fn update_congestion_info_for_cancelled_objects<'a>(
        &self,
        now: Instant,
        congested_objects: impl IntoIterator<Item = &'a ObjectID>,
        gas_price: u64,
    ) {
        for object_id in congested_objects {
            self.congestion_clearing_prices
                .entry(*object_id)
                .and_upsert_with(|maybe_entry| {
                    if let Some(e) = maybe_entry {
                        let mut e = e.into_value();
                        e.update_for_cancellation(now, gas_price);
                        e
                    } else {
                        CongestionInfo {
                            last_cancellation_time: now,
                            highest_cancelled_gas_price: gas_price,
                            last_success_time: None,
                            lowest_executed_gas_price: None,
                        }
                    }
                });
        }
    }

    fn update_congestion_info_for_successful_transaction<'a>(
        &self,
        now: Instant,
        shared_inputs: impl IntoIterator<Item = &'a ObjectID>,
        gas_price: u64,
    ) {
        // iterate over all mutable shared inputs
        for object in shared_inputs {
            self.congestion_clearing_prices
                .entry(*object)
                .and_compute_with(|maybe_entry| {
                    if let Some(e) = maybe_entry {
                        let mut e = e.into_value();
                        e.update_for_success(now, gas_price);
                        Op::Put(e)
                    } else {
                        // do not insert info about objects that haven't
                        // had a recent cancellation
                        Op::Nop
                    }
                });
        }
    }
}

impl Default for CongestionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl CongestionInfo {
    fn update_for_cancellation(&mut self, now: Instant, gas_price: u64) {
        self.last_cancellation_time = now;
        self.highest_cancelled_gas_price =
            std::cmp::max(self.highest_cancelled_gas_price, gas_price);
    }

    fn update_for_success(&mut self, now: Instant, gas_price: u64) {
        self.last_success_time = Some(now);
        self.lowest_executed_gas_price = Some(match self.lowest_executed_gas_price {
            Some(current_min) => std::cmp::min(current_min, gas_price),
            None => gas_price,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn test_congestion_tracker() {
        let t1 = Instant::now();
        let tracker = CongestionTracker::new();

        let o = [ObjectID::random(), ObjectID::random(), ObjectID::random()];

        // update congestion info for cancelled objects
        tracker.update_congestion_info_for_cancelled_objects(t1, &[o[0], o[1]], 100);

        // verify congestion info was updated correctly
        for object_id in &[o[0], o[1]] {
            let info = tracker.get_congestion_info(*object_id).unwrap();
            assert_eq!(info.last_cancellation_time, t1);
            assert_eq!(info.highest_cancelled_gas_price, 100);
            assert!(info.last_success_time.is_none());
            assert!(info.lowest_executed_gas_price.is_none());
        }

        let t2 = t1 + Duration::from_secs(1);

        // now a successful transaction comes in
        tracker.update_congestion_info_for_successful_transaction(t2, &[o[0]], 120);

        // verify congestion info was updated correctly
        let info = tracker.get_congestion_info(o[0]).unwrap();
        assert_eq!(info.last_success_time, Some(t2));
        assert_eq!(info.last_cancellation_time, t1);
        assert_eq!(info.lowest_executed_gas_price, Some(120));
        assert_eq!(info.highest_cancelled_gas_price, 100);

        let t3 = t2 + Duration::from_secs(1);

        // another successful transaction happens at a higher gas price
        // lowest_executed_gas_price should not be updated
        tracker.update_congestion_info_for_successful_transaction(t3, &[o[0]], 130);
        let info = tracker.get_congestion_info(o[0]).unwrap();
        assert_eq!(info.last_success_time, Some(t3));
        assert_eq!(info.lowest_executed_gas_price, Some(120));

        // a congested transaction comes in with a lower gas price,
        // highest_cancelled_gas_price should not be updated
        let t4 = t3 + Duration::from_secs(1);
        tracker.update_congestion_info_for_cancelled_objects(t4, &[o[1]], 90);
        let info = tracker.get_congestion_info(o[1]).unwrap();
        assert_eq!(info.last_cancellation_time, t4);
        assert_eq!(info.highest_cancelled_gas_price, 100);

        // a successful transaction comes in with a lower gas price,
        // lowest_executed_gas_price should be updated
        let t5 = t4 + Duration::from_secs(1);
        tracker.update_congestion_info_for_successful_transaction(t5, &[o[1]], 110);
        let info = tracker.get_congestion_info(o[1]).unwrap();
        assert_eq!(info.last_success_time, Some(t5));
        assert_eq!(info.lowest_executed_gas_price, Some(110));
    }
}
