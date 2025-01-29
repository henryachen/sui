// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use async_stream::stream;
use futures::{Stream, StreamExt};
/*
use std::sync::Arc;

use sui_config::node::RunWithRange;
use tokio::sync::broadcast;

use crate::authority::authority_per_epoch_store::AuthorityPerEpochStore;
use crate::authority::AuthorityState;
use crate::checkpoints::checkpoint_executor::CheckpointExecutorConfig;
use crate::checkpoints::checkpoint_executor::CheckpointExecutorMetrics;
use sui_types::full_checkpoint_content::CheckpointData;

use crate::authority::backpressure::BackpressureManager;
use crate::checkpoints::CheckpointStore;
use crate::checkpoints::VerifiedCheckpoint;
use crate::execution_cache::ObjectCacheRead;
use crate::execution_cache::TransactionCacheRead;
use crate::state_accumulator::StateAccumulator;
use crate::transaction_manager::TransactionManager;

use super::StopReason;
*/
use super::*;

pub struct CheckpointExecutorV2 {
    mailbox: broadcast::Receiver<VerifiedCheckpoint>,
    // TODO: AuthorityState is only needed because we have to call deprecated_insert_finalized_transactions
    // once that code is fully deprecated we can remove this
    state: Arc<AuthorityState>,
    checkpoint_store: Arc<CheckpointStore>,
    object_cache_reader: Arc<dyn ObjectCacheRead>,
    transaction_cache_reader: Arc<dyn TransactionCacheRead>,
    tx_manager: Arc<TransactionManager>,
    accumulator: Arc<StateAccumulator>,
    backpressure_manager: Arc<BackpressureManager>,
    config: CheckpointExecutorConfig,
    metrics: Arc<CheckpointExecutorMetrics>,
    subscription_service_checkpoint_sender: Option<tokio::sync::mpsc::Sender<CheckpointData>>,
}

impl CheckpointExecutorV2 {
    pub fn new(
        mailbox: broadcast::Receiver<VerifiedCheckpoint>,
        checkpoint_store: Arc<CheckpointStore>,
        state: Arc<AuthorityState>,
        accumulator: Arc<StateAccumulator>,
        backpressure_manager: Arc<BackpressureManager>,
        config: CheckpointExecutorConfig,
        metrics: Arc<CheckpointExecutorMetrics>,
        subscription_service_checkpoint_sender: Option<tokio::sync::mpsc::Sender<CheckpointData>>,
    ) -> Self {
        Self {
            mailbox,
            state: state.clone(),
            checkpoint_store,
            object_cache_reader: state.get_object_cache_reader().clone(),
            transaction_cache_reader: state.get_transaction_cache_reader().clone(),
            tx_manager: state.transaction_manager().clone(),
            accumulator,
            backpressure_manager,
            config,
            metrics,
            subscription_service_checkpoint_sender,
        }
    }

    pub fn new_for_tests(
        mailbox: broadcast::Receiver<VerifiedCheckpoint>,
        checkpoint_store: Arc<CheckpointStore>,
        state: Arc<AuthorityState>,
        accumulator: Arc<StateAccumulator>,
    ) -> Self {
        Self::new(
            mailbox,
            checkpoint_store,
            state,
            accumulator,
            BackpressureManager::new_for_tests(),
            Default::default(),
            CheckpointExecutorMetrics::new_for_tests(),
            None,
        )
    }

    /// Ensure that all checkpoints in the current epoch will be executed.
    /// We don't technically need &mut on self, but passing it to make sure only one instance is
    /// running at one time.
    #[instrument(level = "error", skip_all, fields(epoch = ?epoch_store.epoch()))]
    pub async fn run_epoch(
        &mut self,
        epoch_store: Arc<AuthorityPerEpochStore>,
        run_with_range: Option<RunWithRange>,
    ) -> StopReason {
        debug!(?run_with_range, "CheckpointExecutor::run_epoch");

        // check if we want to run this epoch based on RunWithRange condition value
        // we want to be inclusive of the defined RunWithRangeEpoch::Epoch
        // i.e Epoch(N) means we will execute epoch N and stop when reaching N+1
        if run_with_range.map_or(false, |rwr| rwr.is_epoch_gt(epoch_store.epoch())) {
            info!("RunWithRange condition satisfied at {:?}", run_with_range,);
            return StopReason::RunWithRangeCondition;
        };

        self.metrics
            .checkpoint_exec_epoch
            .set(epoch_store.epoch() as i64);

        // Decide the first checkpoint to schedule for execution.
        // If we haven't executed anything in the past, we schedule checkpoint 0.
        // Otherwise we schedule the one after highest executed.
        let mut highest_executed = self
            .checkpoint_store
            .get_highest_executed_checkpoint()
            .unwrap();

        if let Some(highest_executed) = &highest_executed {
            if epoch_store.epoch() == highest_executed.epoch()
                && highest_executed.is_last_checkpoint_of_epoch()
            {
                // We can arrive at this point if we bump the highest_executed_checkpoint watermark, and then
                // crash before completing reconfiguration.
                info!(seq = ?highest_executed.sequence_number, "final checkpoint of epoch has already been executed");
                return StopReason::EpochComplete;
            }
        }

        let mut next_to_schedule = highest_executed
            .as_ref()
            .map(|c| c.sequence_number() + 1)
            .unwrap_or_else(|| {
                // TODO this invariant may no longer hold once we introduce snapshots
                assert_eq!(epoch_store.epoch(), 0);
                0
            });

        self.schedule_synced_checkpoints(
            self.checkpoint_store.clone(),
            next_to_schedule,
            run_with_range.and_then(|rwr| rwr.into_checkpoint_bound()),
        )
        // concurrent step
        .map(|checkpoint| async move {
            self.execute_checkpoint(checkpoint).await;
        })
        .buffered(10)
        // sequential step
        .for_each(|_| async move {
            // TODO: implement
        })
        .await;

        return StopReason::EpochComplete;
    }

    #[instrument(level = "debug", skip_all)]
    fn schedule_synced_checkpoints(
        &self,
        checkpoint_store: Arc<CheckpointStore>,
        start_seq: CheckpointSequenceNumber,
        stop_seq: Option<CheckpointSequenceNumber>,
    ) -> impl Stream<Item = VerifiedCheckpoint> + 'static {
        stream! {
            for seq in start_seq..stop_seq.unwrap_or(u64::MAX) {
                let checkpoint = checkpoint_store.notify_read_synced_checkpoint(seq).await;
                // TODO: enforce run_with_range
                if checkpoint.end_of_epoch_data.is_some() {
                    break;
                }
                yield checkpoint;
            }
        }
    }
}
