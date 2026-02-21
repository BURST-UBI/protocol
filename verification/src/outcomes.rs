//! Verification outcome processor — distributes rewards and penalties.
//!
//! After a verification round completes, this module computes:
//! - Endorser rewards (TRST reward on success, nothing on failure)
//! - Correct verifier rewards (stake returned + share of dissenter stakes)
//! - Incorrect verifier penalties (stake forfeited)
//!
//! For challenges:
//! - Successful challenger: stake returned + 2x reward
//! - Failed challenger: stake forfeited

use burst_types::WalletAddress;

/// Outcome of a completed verification round.
#[derive(Clone, Debug)]
pub struct VerificationOutcomeEvent {
    /// The wallet that was being verified.
    pub wallet: WalletAddress,
    /// Whether verification succeeded or failed.
    pub result: VerificationResult,
    /// Outcomes for each endorser.
    pub endorsers: Vec<EndorserOutcome>,
    /// Outcomes for each verifier.
    pub verifiers: Vec<VerifierOutcome>,
}

/// The result of a verification round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerificationResult {
    /// The wallet was successfully verified as a unique human.
    Verified,
    /// Verification failed — the wallet was not confirmed as a unique human.
    Failed,
}

/// Outcome for a single endorser in a verification round.
#[derive(Clone, Debug)]
pub struct EndorserOutcome {
    /// The endorser's wallet address.
    pub address: WalletAddress,
    /// Amount of BRN permanently burned for the endorsement.
    pub brn_burned: u128,
    /// TRST reward on successful verification (10% of burn amount).
    pub trst_reward: u128,
}

/// Outcome for a single verifier in a verification round.
#[derive(Clone, Debug)]
pub struct VerifierOutcome {
    /// The verifier's wallet address.
    pub address: WalletAddress,
    /// Amount of BRN staked for this verification.
    pub staked: u128,
    /// Whether this verifier voted with the majority outcome.
    pub voted_correctly: bool,
    /// Reward: stake returned + share of dissenter stakes (correct voters only).
    pub reward: u128,
    /// Penalty: stake forfeited (incorrect voters only).
    pub penalty: u128,
}

/// Default endorser reward ratio in basis points (1000 = 10%).
pub const DEFAULT_ENDORSER_REWARD_BPS: u32 = 1000;

/// Process a completed verification and compute rewards/penalties.
///
/// Endorsers receive a TRST reward (default 10% of burn amount) on success.
/// Correct verifiers get their stake back plus an equal share of all dissenter stakes.
/// Incorrect verifiers lose their entire stake.
pub fn compute_verification_outcomes(
    wallet: &WalletAddress,
    result: VerificationResult,
    endorsers: &[(WalletAddress, u128)],
    verifiers: &[(WalletAddress, u128, bool)],
) -> VerificationOutcomeEvent {
    compute_verification_outcomes_with_reward(
        wallet,
        result,
        endorsers,
        verifiers,
        DEFAULT_ENDORSER_REWARD_BPS,
    )
}

/// Process verification outcomes with a configurable endorser reward ratio.
pub fn compute_verification_outcomes_with_reward(
    wallet: &WalletAddress,
    result: VerificationResult,
    endorsers: &[(WalletAddress, u128)],
    verifiers: &[(WalletAddress, u128, bool)],
    endorser_reward_bps: u32,
) -> VerificationOutcomeEvent {
    let total_dissenter_stakes: u128 = verifiers
        .iter()
        .filter(|(_, _, correct)| !correct)
        .map(|(_, staked, _)| staked)
        .sum();

    // Only verifiers who staked (stake > 0) are eligible for reward distribution.
    // Neither voters contribute 0 stake and must not receive shares.
    let staked_correct_count = verifiers
        .iter()
        .filter(|(_, staked, correct)| *correct && *staked > 0)
        .count() as u128;
    let reward_per_correct = total_dissenter_stakes
        .checked_div(staked_correct_count)
        .unwrap_or(0);

    let endorser_outcomes: Vec<EndorserOutcome> = endorsers
        .iter()
        .map(|(addr, burned)| {
            let trst_reward = match result {
                VerificationResult::Verified => *burned * endorser_reward_bps as u128 / 10_000,
                VerificationResult::Failed => 0,
            };
            EndorserOutcome {
                address: addr.clone(),
                brn_burned: *burned,
                trst_reward,
            }
        })
        .collect();

    let verifier_outcomes: Vec<VerifierOutcome> = verifiers
        .iter()
        .map(|(addr, staked, correct)| {
            if *correct && *staked > 0 {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: *staked,
                    voted_correctly: true,
                    reward: staked + reward_per_correct,
                    penalty: 0,
                }
            } else if *correct {
                // Correct but zero stake (Neither voters) — no reward
                VerifierOutcome {
                    address: addr.clone(),
                    staked: 0,
                    voted_correctly: true,
                    reward: 0,
                    penalty: 0,
                }
            } else {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: *staked,
                    voted_correctly: false,
                    reward: 0,
                    penalty: *staked,
                }
            }
        })
        .collect();

    VerificationOutcomeEvent {
        wallet: wallet.clone(),
        result,
        endorsers: endorser_outcomes,
        verifiers: verifier_outcomes,
    }
}

/// Outcome of a completed challenge.
#[derive(Clone, Debug)]
pub struct ChallengeOutcomeEvent {
    /// The wallet that was challenged.
    pub challenged_wallet: WalletAddress,
    /// The wallet that submitted the challenge.
    pub challenger: WalletAddress,
    /// Whether fraud was confirmed or the challenge was rejected.
    pub outcome: ChallengeResult,
    /// The BRN stake the challenger put up.
    pub challenger_stake: u128,
    /// TRST reward for successful challengers (2x stake).
    pub challenger_reward: u128,
    /// Outcomes for each verifier in the challenge vote.
    pub verifier_outcomes: Vec<VerifierOutcome>,
}

/// The result of a challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChallengeResult {
    /// Fraud was confirmed — the challenged wallet is de-verified.
    FraudConfirmed,
    /// The challenge was rejected — the challenged wallet remains verified.
    ChallengeRejected,
    /// Challenge expired without enough votes — resolved in favor of the challenged wallet.
    /// Challenger's stake is returned minus a penalty for wasting network time.
    Expired,
}

/// Compute the outcome of a challenge.
///
/// If fraud is confirmed, the challenger receives 2x their stake as reward.
/// If the challenge is rejected, the challenger's stake is forfeited.
/// Verifier outcomes follow the same reward/penalty logic as regular verification.
pub fn compute_challenge_outcome(
    challenged: &WalletAddress,
    challenger: &WalletAddress,
    outcome: ChallengeResult,
    stake: u128,
    verifiers: &[(WalletAddress, u128, bool)],
) -> ChallengeOutcomeEvent {
    let total_dissenter_stakes: u128 = verifiers
        .iter()
        .filter(|(_, _, correct)| !correct)
        .map(|(_, staked, _)| staked)
        .sum();

    let staked_correct_count = verifiers
        .iter()
        .filter(|(_, staked, correct)| *correct && *staked > 0)
        .count() as u128;
    let reward_per_correct = total_dissenter_stakes
        .checked_div(staked_correct_count)
        .unwrap_or(0);

    let verifier_outcomes: Vec<VerifierOutcome> = verifiers
        .iter()
        .map(|(addr, staked, correct)| {
            if *correct && *staked > 0 {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: *staked,
                    voted_correctly: true,
                    reward: staked + reward_per_correct,
                    penalty: 0,
                }
            } else if *correct {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: 0,
                    voted_correctly: true,
                    reward: 0,
                    penalty: 0,
                }
            } else {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: *staked,
                    voted_correctly: false,
                    reward: 0,
                    penalty: *staked,
                }
            }
        })
        .collect();

    match outcome {
        ChallengeResult::FraudConfirmed => ChallengeOutcomeEvent {
            challenged_wallet: challenged.clone(),
            challenger: challenger.clone(),
            outcome: ChallengeResult::FraudConfirmed,
            challenger_stake: stake,
            challenger_reward: stake * 2,
            verifier_outcomes,
        },
        ChallengeResult::ChallengeRejected => ChallengeOutcomeEvent {
            challenged_wallet: challenged.clone(),
            challenger: challenger.clone(),
            outcome: ChallengeResult::ChallengeRejected,
            challenger_stake: stake,
            challenger_reward: 0,
            verifier_outcomes,
        },
        ChallengeResult::Expired => ChallengeOutcomeEvent {
            challenged_wallet: challenged.clone(),
            challenger: challenger.clone(),
            outcome: ChallengeResult::Expired,
            challenger_stake: stake,
            challenger_reward: stake / 2, // return half; other half is penalty for wasting time
            verifier_outcomes,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address(n: u8) -> WalletAddress {
        WalletAddress::new(format!("brst_{:0>60}", n))
    }

    // ── Verification outcome tests ──────────────────────────────────────

    #[test]
    fn verified_endorsers_get_10_percent_trst_reward() {
        let wallet = test_address(1);
        let endorsers = vec![(test_address(10), 1000u128), (test_address(11), 2000u128)];
        let verifiers = vec![
            (test_address(20), 500u128, true),
            (test_address(21), 500u128, true),
        ];

        let outcome = compute_verification_outcomes(
            &wallet,
            VerificationResult::Verified,
            &endorsers,
            &verifiers,
        );

        assert_eq!(outcome.result, VerificationResult::Verified);
        assert_eq!(outcome.endorsers.len(), 2);
        assert_eq!(outcome.endorsers[0].trst_reward, 100); // 10% of 1000
        assert_eq!(outcome.endorsers[1].trst_reward, 200); // 10% of 2000
    }

    #[test]
    fn failed_endorsers_get_no_reward() {
        let wallet = test_address(1);
        let endorsers = vec![(test_address(10), 1000u128)];
        let verifiers = vec![(test_address(20), 500u128, true)];

        let outcome = compute_verification_outcomes(
            &wallet,
            VerificationResult::Failed,
            &endorsers,
            &verifiers,
        );

        assert_eq!(outcome.result, VerificationResult::Failed);
        assert_eq!(outcome.endorsers[0].trst_reward, 0);
        assert_eq!(outcome.endorsers[0].brn_burned, 1000);
    }

    #[test]
    fn correct_verifiers_split_dissenter_stakes() {
        let wallet = test_address(1);
        let endorsers = vec![];
        let verifiers = vec![
            (test_address(20), 500u128, true),  // correct
            (test_address(21), 500u128, true),  // correct
            (test_address(22), 600u128, false), // dissenter
            (test_address(23), 400u128, false), // dissenter
        ];

        let outcome = compute_verification_outcomes(
            &wallet,
            VerificationResult::Verified,
            &endorsers,
            &verifiers,
        );

        // Total dissenter stakes: 600 + 400 = 1000
        // 2 correct verifiers: each gets 1000 / 2 = 500
        assert_eq!(outcome.verifiers[0].reward, 500 + 500); // stake + share
        assert_eq!(outcome.verifiers[0].penalty, 0);
        assert_eq!(outcome.verifiers[1].reward, 500 + 500);
        assert_eq!(outcome.verifiers[1].penalty, 0);

        // Dissenters lose their stakes
        assert_eq!(outcome.verifiers[2].reward, 0);
        assert_eq!(outcome.verifiers[2].penalty, 600);
        assert_eq!(outcome.verifiers[3].reward, 0);
        assert_eq!(outcome.verifiers[3].penalty, 400);
    }

    #[test]
    fn all_verifiers_correct_no_dissenter_reward() {
        let wallet = test_address(1);
        let endorsers = vec![];
        let verifiers = vec![
            (test_address(20), 500u128, true),
            (test_address(21), 500u128, true),
        ];

        let outcome = compute_verification_outcomes(
            &wallet,
            VerificationResult::Verified,
            &endorsers,
            &verifiers,
        );

        // No dissenters — reward equals only the stake itself
        assert_eq!(outcome.verifiers[0].reward, 500);
        assert_eq!(outcome.verifiers[1].reward, 500);
    }

    #[test]
    fn all_verifiers_incorrect_no_reward_all_penalty() {
        let wallet = test_address(1);
        let endorsers = vec![];
        let verifiers = vec![
            (test_address(20), 500u128, false),
            (test_address(21), 500u128, false),
        ];

        let outcome = compute_verification_outcomes(
            &wallet,
            VerificationResult::Failed,
            &endorsers,
            &verifiers,
        );

        // No correct verifiers — all stakes forfeited
        assert_eq!(outcome.verifiers[0].reward, 0);
        assert_eq!(outcome.verifiers[0].penalty, 500);
        assert_eq!(outcome.verifiers[1].reward, 0);
        assert_eq!(outcome.verifiers[1].penalty, 500);
    }

    #[test]
    fn single_correct_verifier_gets_all_dissenter_stakes() {
        let wallet = test_address(1);
        let endorsers = vec![];
        let verifiers = vec![
            (test_address(20), 500u128, true), // only correct
            (test_address(21), 300u128, false),
            (test_address(22), 400u128, false),
            (test_address(23), 300u128, false),
        ];

        let outcome = compute_verification_outcomes(
            &wallet,
            VerificationResult::Verified,
            &endorsers,
            &verifiers,
        );

        // Total dissenter: 300 + 400 + 300 = 1000
        // 1 correct verifier gets all 1000
        assert_eq!(outcome.verifiers[0].reward, 500 + 1000);
        assert_eq!(outcome.verifiers[0].penalty, 0);
    }

    #[test]
    fn empty_verifiers_and_endorsers() {
        let wallet = test_address(1);
        let outcome =
            compute_verification_outcomes(&wallet, VerificationResult::Verified, &[], &[]);

        assert_eq!(outcome.wallet, wallet);
        assert_eq!(outcome.result, VerificationResult::Verified);
        assert!(outcome.endorsers.is_empty());
        assert!(outcome.verifiers.is_empty());
    }

    #[test]
    fn verification_outcome_preserves_wallet() {
        let wallet = test_address(42);
        let outcome = compute_verification_outcomes(&wallet, VerificationResult::Failed, &[], &[]);
        assert_eq!(outcome.wallet, wallet);
    }

    // ── Challenge outcome tests ─────────────────────────────────────────

    #[test]
    fn fraud_confirmed_gives_double_stake_reward() {
        let challenged = test_address(1);
        let challenger = test_address(2);
        let stake = 1000u128;
        let verifiers = vec![
            (test_address(30), 500u128, true),
            (test_address(31), 500u128, false),
        ];

        let outcome = compute_challenge_outcome(
            &challenged,
            &challenger,
            ChallengeResult::FraudConfirmed,
            stake,
            &verifiers,
        );

        assert_eq!(outcome.outcome, ChallengeResult::FraudConfirmed);
        assert_eq!(outcome.challenger_stake, 1000);
        assert_eq!(outcome.challenger_reward, 2000);
        assert_eq!(outcome.challenged_wallet, challenged);
        assert_eq!(outcome.challenger, challenger);
        assert_eq!(outcome.verifier_outcomes.len(), 2);
        assert!(outcome.verifier_outcomes[0].voted_correctly);
        assert_eq!(outcome.verifier_outcomes[0].reward, 500 + 500);
        assert!(!outcome.verifier_outcomes[1].voted_correctly);
        assert_eq!(outcome.verifier_outcomes[1].penalty, 500);
    }

    #[test]
    fn challenge_rejected_forfeits_stake() {
        let challenged = test_address(1);
        let challenger = test_address(2);
        let stake = 1000u128;

        let outcome = compute_challenge_outcome(
            &challenged,
            &challenger,
            ChallengeResult::ChallengeRejected,
            stake,
            &[],
        );

        assert_eq!(outcome.outcome, ChallengeResult::ChallengeRejected);
        assert_eq!(outcome.challenger_stake, 1000);
        assert_eq!(outcome.challenger_reward, 0);
        assert!(outcome.verifier_outcomes.is_empty());
    }

    #[test]
    fn challenge_with_zero_stake() {
        let challenged = test_address(1);
        let challenger = test_address(2);

        let outcome = compute_challenge_outcome(
            &challenged,
            &challenger,
            ChallengeResult::FraudConfirmed,
            0,
            &[],
        );

        assert_eq!(outcome.challenger_stake, 0);
        assert_eq!(outcome.challenger_reward, 0);
    }

    #[test]
    fn challenge_preserves_addresses() {
        let challenged = test_address(10);
        let challenger = test_address(20);

        let outcome = compute_challenge_outcome(
            &challenged,
            &challenger,
            ChallengeResult::FraudConfirmed,
            500,
            &[],
        );

        assert_eq!(outcome.challenged_wallet, challenged);
        assert_eq!(outcome.challenger, challenger);
    }
}
