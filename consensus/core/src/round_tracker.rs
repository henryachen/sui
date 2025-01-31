use std::sync::Arc;

use consensus_config::{AuthorityIndex, Committee};

use crate::{
    block::{BlockAPI, ExtendedBlock},
    context::Context,
    BlockRef, Round, VerifiedBlock,
};

pub(crate) struct PeerRoundTracker {
    /// Highest accepted round per authority from received blocks (included/excluded ancestors)
    block_accepted_rounds: Vec<Vec<Round>>,
    /// Highest accepted round per authority from round prober
    probed_accepted_rounds: Vec<Vec<Round>>,
    /// Highest received round per authority from round prober
    probed_received_rounds: Vec<Vec<Round>>,
    context: Arc<Context>,
}

/// A [`QuorumRound`] is a round range [low, high]. It is computed from
/// highest received or accepted rounds of an authority reported by all
/// authorities.
/// The bounds represent:
/// - the highest round lower or equal to rounds from a quorum (low)
/// - the lowest round higher or equal to rounds from a quorum (high)
///
/// [`QuorumRound`] is useful because:
/// - [low, high] range is BFT, always between the lowest and highest rounds
///   of honest validators, with < validity threshold of malicious stake.
/// - It provides signals about how well blocks from an authority propagates
///   in the network. If low bound for an authority is lower than its last
///   proposed round, the last proposed block has not propagated to a quorum.
///   If a new block is proposed from the authority, it will not get accepted
///   immediately by a quorum.
pub(crate) type QuorumRound = (Round, Round);

impl PeerRoundTracker {
    pub fn new(context: Arc<Context>) -> Self {
        let size = context.committee.size();
        Self {
            block_accepted_rounds: vec![vec![0; size]; size],
            probed_accepted_rounds: vec![vec![0; size]; size],
            probed_received_rounds: vec![vec![0; size]; size],
            context,
        }
    }

    /// Update accepted rounds based on a new block and its excluded ancestors
    pub fn update_from_block(&mut self, extended_block: ExtendedBlock) {
        let block = extended_block.block;
        let excluded_ancestors = extended_block.excluded_ancestors;
        let author = block.author();

        // Update peers block round
        self.block_accepted_rounds[author][author] =
            self.block_accepted_rounds[author][author].max(block.round());

        // Update accepted rounds from included ancestors
        for ancestor in block.ancestors() {
            self.block_accepted_rounds[author][ancestor.author] =
                self.block_accepted_rounds[author][ancestor.author].max(ancestor.round);
        }

        // Update accepted rounds from excluded ancestors
        for ancestor in excluded_ancestors {
            self.block_accepted_rounds[author][ancestor.author] =
                self.block_accepted_rounds[author][ancestor.author].max(ancestor.round);
        }
    }
    /// Update accepted & received rounds based on probing results
    pub fn update_from_probe(
        &mut self,
        accepted_rounds: Vec<Vec<Round>>,
        received_rounds: Vec<Vec<Round>>,
    ) {
        self.probed_accepted_rounds = accepted_rounds;
        self.probed_received_rounds = received_rounds;
    }

    /// For the peer specified with target_index, compute and return its [`QuorumRound`].
    fn compute_quorum_round(
        committee: &Committee,
        target_index: AuthorityIndex,
        highest_received_rounds: &[Vec<Round>],
    ) -> QuorumRound {
        let mut rounds_with_stake = highest_received_rounds
            .iter()
            .zip(committee.authorities())
            .map(|(rounds, (_, authority))| (rounds[target_index], authority.stake))
            .collect::<Vec<_>>();
        rounds_with_stake.sort();

        // Forward iteration and stopping at validity threshold would produce the same result currently,
        // with fault tolerance of f/3f+1 votes. But it is not semantically correct, and will provide an
        // incorrect value when fault tolerance and validity threshold are different.
        let mut total_stake = 0;
        let mut low = 0;
        for (round, stake) in rounds_with_stake.iter().rev() {
            total_stake += stake;
            if total_stake >= committee.quorum_threshold() {
                low = *round;
                break;
            }
        }

        let mut total_stake = 0;
        let mut high = 0;
        for (round, stake) in rounds_with_stake.iter() {
            total_stake += stake;
            if total_stake >= committee.quorum_threshold() {
                high = *round;
                break;
            }
        }

        (low, high)
    }

    // /// Get the highest accepted rounds combining both probed and block-derived information
    // pub fn get_accepted_rounds(&self) -> Vec<(AuthorityIndex, Round)> {
    //     self.context
    //         .committee
    //         .authorities()
    //         .map(|(peer, _)| {
    //             let block_round =
    //                 self.get_quorum_round_from_matrix(&self.block_accepted_rounds, peer);
    //             let probed_round =
    //                 self.get_quorum_round_from_matrix(&self.probed_accepted_rounds, peer);
    //             (peer, block_round.max(probed_round))
    //         })
    //         .collect()
    // }

    // /// Get the highest received rounds from probing
    // pub fn get_received_rounds(&self) -> Vec<(AuthorityIndex, Round)> {
    //     self.get_quorum_rounds_from_matrix(&self.probed_received_rounds)
    // }

    // // Helper method to compute quorum rounds from a matrix
    // fn get_quorum_round_from_matrix(
    //     &self,
    //     matrix: &[Vec<Round>],
    //     authority: AuthorityIndex,
    // ) -> Round {
    //     // TODO: Implement quorum calculation logic
    //     // This should follow the same logic currently used in round_prober.rs
    //     unimplemented!()
    // }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO: Add tests
}
