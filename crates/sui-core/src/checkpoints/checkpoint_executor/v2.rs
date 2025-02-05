// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use async_stream::stream;
use futures::{Stream, StreamExt};
use mysten_common::fatal;
use sui_types::{messages_checkpoint::CheckpointContents, transaction::Transaction};
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
        let highest_executed = self
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

        let next_to_schedule = highest_executed
            .as_ref()
            .map(|c| c.sequence_number() + 1)
            .unwrap_or_else(|| {
                // TODO this invariant may no longer hold once we introduce snapshots
                assert_eq!(epoch_store.epoch(), 0);
                0
            });

        let mut now_time = Instant::now();
        let mut now_transaction_num = highest_executed
            .as_ref()
            .map(|c| c.network_total_transactions)
            .unwrap_or(0);

        self.schedule_synced_checkpoints(
            self.checkpoint_store.clone(),
            next_to_schedule,
            run_with_range.and_then(|rwr| rwr.into_checkpoint_bound()),
        )
        // concurrent step
        .map(|checkpoint| {
            let checkpoint_store = self.checkpoint_store.clone();
            let transaction_cache_reader = self.transaction_cache_reader.clone();
            let object_cache_reader = self.object_cache_reader.clone();
            let accumulator = self.accumulator.clone();
            let epoch_store = epoch_store.clone();
            let tx_manager = self.tx_manager.clone();
            async move {
                // do synchronous work in a blocking task
                let task = tokio::task::spawn_blocking({
                    let tx_manager = tx_manager.clone();
                    let transaction_cache_reader = transaction_cache_reader.clone();
                    move || {
                        let sequence_number = checkpoint.sequence_number;

                        let (
                            checkpoint_contents,
                            tx_digests,
                            fx_digests,
                            txns,
                            effects,
                            executed_fx_digests,
                        ) = Self::load_checkpoint_transactions(
                            &checkpoint,
                            &checkpoint_store,
                            &*transaction_cache_reader,
                        );

                        let unexecuted_tx_digests = Self::execute_checkpoint(
                            epoch_store.as_ref(),
                            tx_manager.as_ref(),
                            transaction_cache_reader.as_ref(),
                            &checkpoint,
                            &tx_digests,
                            &fx_digests,
                            txns.clone(),
                            &executed_fx_digests,
                        );

                        let checkpoint_acc = accumulator
                            .accumulate_checkpoint(effects, sequence_number, &epoch_store)
                            .expect("epoch cannot have ended");

                        let checkpoint_data = load_checkpoint_data(
                            checkpoint,
                            checkpoint_contents,
                            txns,
                            effects,
                            &*object_cache_reader,
                            &*transaction_cache_reader,
                            &tx_digests,
                        )
                        .expect("failed to load checkpoint data");

                        (
                            tx_digests,
                            unexecuted_tx_digests,
                            checkpoint_acc,
                            checkpoint_data,
                        )
                    }
                });
                let (tx_digests, unexecuted_tx_digests, checkpoint_acc, checkpoint_data) =
                    task.await.unwrap();
                transaction_cache_reader
                    .notify_read_executed_effects_digests(&unexecuted_tx_digests)
                    .await;

                todo!("write checkpoint data");
                //self.write_checkpoint_data(checkpoint).await;
                (tx_digests, checkpoint, checkpoint_acc, checkpoint_data)
            }
        })
        .buffered(10)
        // sequential step
        .for_each(
            |(tx_digests, checkpoint, checkpoint_acc, checkpoint_data)| async move {
                // TODO: implement

                let _process_scope = mysten_metrics::monitored_scope("ProcessExecutedCheckpoint");

                self.process_executed_checkpoint(
                    &epoch_store,
                    &checkpoint,
                    checkpoint_acc,
                    checkpoint_data,
                    &tx_digests,
                )
                .await;
                self.backpressure_manager
                    .update_highest_executed_checkpoint(*checkpoint.sequence_number());
                highest_executed = Some(checkpoint.clone());

                // Estimate TPS every 10k transactions or 30 sec
                let elapsed = now_time.elapsed().as_millis();
                let current_transaction_num = highest_executed
                    .as_ref()
                    .map(|c| c.network_total_transactions)
                    .unwrap_or(0);
                if current_transaction_num - now_transaction_num > 10_000 || elapsed > 30_000 {
                    let tps = (1000.0 * (current_transaction_num - now_transaction_num) as f64
                        / elapsed as f64) as i32;
                    self.metrics.checkpoint_exec_sync_tps.set(tps as i64);
                    now_time = Instant::now();
                    now_transaction_num = current_transaction_num;
                }
            },
        )
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

impl CheckpointExecutorV2 {
    fn load_checkpoint_transactions(
        checkpoint: &VerifiedCheckpoint,
        checkpoint_store: &Arc<CheckpointStore>,
        transaction_cache_reader: &dyn TransactionCacheRead,
    ) -> (
        CheckpointContents,
        Vec<TransactionDigest>,
        Vec<TransactionEffectsDigest>,
        Vec<VerifiedExecutableTransaction>,
        Vec<TransactionEffects>,
        Vec<Option<TransactionEffectsDigest>>,
    ) {
        let seq = checkpoint.sequence_number;
        let epoch = checkpoint.epoch;

        let checkpoint_contents = checkpoint_store
            .get_checkpoint_contents(&checkpoint.content_digest)
            .expect("db error")
            .expect("checkpoint contents not found");

        // attempt to load full checkpoint contents in bulk
        if let Some(full_contents) = checkpoint_store
            .get_full_checkpoint_contents_by_sequence_number(seq)
            .expect("Failed to get checkpoint contents from store")
            .tap_some(|_| debug!("loaded full checkpoint contents in bulk for sequence {seq}"))
        {
            let num_txns = full_contents.size();
            let mut tx_digests = Vec::with_capacity(num_txns);
            let mut txns = Vec::with_capacity(num_txns);
            let mut effects = Vec::with_capacity(num_txns);
            let mut fx_digests = Vec::with_capacity(num_txns);

            full_contents
                .into_iter()
                .zip(checkpoint_contents.iter())
                .for_each(|(execution_data, digests)| {
                    let tx_digest = digests.transaction;
                    let fx_digest = digests.effects;
                    debug_assert_eq!(tx_digest, *execution_data.transaction.digest());
                    debug_assert_eq!(fx_digest, execution_data.effects.digest());

                    tx_digests.push(tx_digest);
                    txns.push(VerifiedExecutableTransaction::new_from_checkpoint(
                        VerifiedTransaction::new_unchecked(execution_data.transaction),
                        epoch,
                        seq,
                    ));
                    effects.push(execution_data.effects);
                    fx_digests.push(fx_digest);
                });

            let executed_fx_digests =
                transaction_cache_reader.multi_get_executed_effects_digests(&tx_digests);

            (
                checkpoint_contents,
                tx_digests,
                fx_digests,
                txns,
                effects,
                executed_fx_digests,
            )
        } else {
            // load items one-by-one

            let digests = checkpoint_contents.inner();

            let (tx_digests, fx_digests): (Vec<_>, Vec<_>) =
                digests.iter().map(|d| (d.transaction, d.effects)).unzip();
            let transactions = transaction_cache_reader
                .multi_get_transaction_blocks(&tx_digests)
                .into_iter()
                .enumerate()
                .map(|(i, tx)| {
                    let tx = tx
                        .unwrap_or_else(|| fatal!("transaction not found for {:?}", tx_digests[i]));
                    let tx = Arc::try_unwrap(tx).unwrap_or_else(|tx| (*tx).clone());
                    VerifiedExecutableTransaction::new_from_checkpoint(tx, epoch, seq)
                })
                .collect();
            let effects = transaction_cache_reader
                .multi_get_effects(&fx_digests)
                .into_iter()
                .enumerate()
                .map(|(i, effect)| {
                    effect.unwrap_or_else(|| {
                        fatal!("checkpoint effect not found for {:?}", digests[i])
                    })
                })
                .collect();

            let executed_fx_digests =
                transaction_cache_reader.multi_get_executed_effects_digests(&tx_digests);

            (
                checkpoint_contents,
                tx_digests,
                fx_digests,
                transactions,
                effects,
                executed_fx_digests,
            )
        }
    }

    fn execute_checkpoint(
        epoch_store: &AuthorityPerEpochStore,
        tx_manager: &TransactionManager,
        transaction_cache_reader: &dyn TransactionCacheRead,
        object_cache_reader: &dyn ObjectCacheRead,
        checkpoint: &VerifiedCheckpoint,
        tx_digests: &[TransactionDigest],
        fx_digests: &[TransactionEffectsDigest],
        txns: Vec<VerifiedExecutableTransaction>,
        effects: &[TransactionEffects],
        executed_fx_digests: &[Option<TransactionEffectsDigest>],
    ) -> Vec<TransactionDigest> {
        // Find unexecuted transactions and their expected effects digests
        // Check that all transactions that are already executed have the correct effects digests
        let (unexecuted_tx_digests, unexecuted_txns, unexecuted_effects): (Vec<_>, Vec<_>, Vec<_>) =
            itertools::multiunzip(
                itertools::izip!(txns, tx_digests, fx_digests, effects, executed_fx_digests)
                    .filter_map(
                        |(txn, tx_digest, expected_fx_digest, effects, executed_fx_digest)| {
                            if let Some(executed_fx_digest) = executed_fx_digest {
                                assert_not_forked(
                                    checkpoint,
                                    &tx_digest,
                                    &expected_fx_digest,
                                    &executed_fx_digest,
                                    transaction_cache_reader,
                                );
                                None
                            } else {
                                Some((tx_digest, (txn, *expected_fx_digest), effects))
                            }
                        },
                    ),
            );

        for ((tx, _), effects) in itertools::izip!(unexecuted_txns, unexecuted_effects) {
            if tx.contains_shared_object() {
                epoch_store.acquire_shared_version_assignments_from_effects(
                    &tx,
                    effects,
                    object_cache_reader,
                )?;
            }
        }

        // Enqueue unexecuted transactions with their expected effects digests
        tx_manager.enqueue_with_expected_effects_digest(unexecuted_txns, &epoch_store);

        unexecuted_tx_digests
    }

    /// Post processing and plumbing after we executed a checkpoint. This function is guaranteed
    /// to be called in the order of checkpoint sequence number.
    #[instrument(level = "debug", skip_all)]
    async fn process_executed_checkpoint(
        &self,
        epoch_store: &AuthorityPerEpochStore,
        checkpoint: &VerifiedCheckpoint,
        checkpoint_acc: Accumulator,
        checkpoint_data: CheckpointData,
        all_tx_digests: &[TransactionDigest],
    ) {
        // Commit all transaction effects to disk
        let cache_commit = self.state.get_cache_commit();
        debug!(seq = ?checkpoint.sequence_number, "committing checkpoint transactions to disk");
        cache_commit
            .commit_transaction_outputs(
                epoch_store.epoch(),
                all_tx_digests,
                epoch_store
                    .protocol_config()
                    .use_object_per_epoch_marker_table_v2_as_option()
                    .unwrap_or(false),
            )
            .await;

        epoch_store
            .handle_committed_transactions(all_tx_digests)
            .expect("cannot fail");

        self.commit_index_updates_and_enqueue_to_subscription_service(checkpoint_data)
            .await;

        if !checkpoint.is_last_checkpoint_of_epoch() {
            self.accumulator
                .accumulate_running_root(
                    epoch_store,
                    checkpoint.sequence_number,
                    Some(checkpoint_acc),
                )
                .await
                .expect("Failed to accumulate running root");
            self.bump_highest_executed_checkpoint(checkpoint);
        }
    }

    fn bump_highest_executed_checkpoint(&self, checkpoint: &VerifiedCheckpoint) {
        // Ensure that we are not skipping checkpoints at any point
        let seq = *checkpoint.sequence_number();
        debug!("Bumping highest_executed_checkpoint watermark to {seq:?}");
        if let Some(prev_highest) = self
            .checkpoint_store
            .get_highest_executed_checkpoint_seq_number()
            .unwrap()
        {
            assert_eq!(prev_highest + 1, seq);
        } else {
            assert_eq!(seq, 0);
        }
        if seq % CHECKPOINT_PROGRESS_LOG_COUNT_INTERVAL == 0 {
            info!("Finished syncing and executing checkpoint {}", seq);
        }

        fail_point!("highest-executed-checkpoint");

        // We store a fixed number of additional FullCheckpointContents after execution is complete
        // for use in state sync.
        const NUM_SAVED_FULL_CHECKPOINT_CONTENTS: u64 = 5_000;
        if seq >= NUM_SAVED_FULL_CHECKPOINT_CONTENTS {
            let prune_seq = seq - NUM_SAVED_FULL_CHECKPOINT_CONTENTS;
            if let Some(prune_checkpoint) = self
                .checkpoint_store
                .get_checkpoint_by_sequence_number(prune_seq)
                .expect("Failed to fetch checkpoint")
            {
                self.checkpoint_store
                    .delete_full_checkpoint_contents(prune_seq)
                    .expect("Failed to delete full checkpoint contents");
                self.checkpoint_store
                    .delete_contents_digest_sequence_number_mapping(
                        &prune_checkpoint.content_digest,
                    )
                    .expect("Failed to delete contents digest -> sequence number mapping");
            } else {
                // If this is directly after a snapshot restore with skiplisting,
                // this is expected for the first `NUM_SAVED_FULL_CHECKPOINT_CONTENTS`
                // checkpoints.
                debug!(
                    "Failed to fetch checkpoint with sequence number {:?}",
                    prune_seq
                );
            }
        }

        self.checkpoint_store
            .update_highest_executed_checkpoint(checkpoint)
            .unwrap();
        self.metrics.last_executed_checkpoint.set(seq as i64);

        self.metrics
            .last_executed_checkpoint_timestamp_ms
            .set(checkpoint.timestamp_ms as i64);
        checkpoint.report_checkpoint_age(
            &self.metrics.last_executed_checkpoint_age,
            &self.metrics.last_executed_checkpoint_age_ms,
        );
    }

    /// If configured, commit the pending index updates for the provided checkpoint as well as
    /// enqueuing the checkpoint to the subscription service
    async fn commit_index_updates_and_enqueue_to_subscription_service(
        &self,
        checkpoint: CheckpointData,
    ) {
        if let Some(rpc_index) = &self.state.rpc_index {
            rpc_index
                .commit_update_for_checkpoint(checkpoint.checkpoint_summary.sequence_number)
                .expect("failed to update rpc_indexes");
        }

        if let Some(sender) = &self.subscription_service_checkpoint_sender {
            if let Err(e) = sender.send(checkpoint).await {
                tracing::warn!("unable to send checkpoint to subscription service: {e}");
            }
        }
    }
}

fn assert_not_forked(
    checkpoint: &VerifiedCheckpoint,
    tx_digest: &TransactionDigest,
    expected_digest: &TransactionEffectsDigest,
    actual_effects_digest: &TransactionEffectsDigest,
    cache_reader: &dyn TransactionCacheRead,
) {
    if *expected_digest != *actual_effects_digest {
        let actual_effects = cache_reader
            .get_executed_effects(tx_digest)
            .expect("actual effects should exist");

        // log observed effects (too big for panic message) and then panic.
        error!(
            ?checkpoint,
            ?tx_digest,
            ?expected_digest,
            ?actual_effects,
            "fork detected!"
        );
        panic!(
            "When executing checkpoint {}, transaction {} \
            is expected to have effects digest {}, but got {}!",
            checkpoint.sequence_number(),
            tx_digest,
            expected_digest,
            actual_effects_digest,
        );
    }
}
