//! Verification outcome processor — distributes rewards and penalties.
//!
//! After a verification round completes, this module computes:
//! - Endorser outcomes (burn recorded; NO protocol reward — decision 33.8a,
//!   endorsement is a social obligation)
//! - Correct verifier outcomes: stake returned + a TRST reward funded exactly
//!   by the forfeited dissenter stakes (decision 33.7d) — TRST is only ever
//!   created from burned BRN, and the forfeited stakes are that burn
//! - Incorrect verifier penalties (stake forfeited/burned)
//!
//! For challenges:
//! - Successful challenger: stake returned; the TRST reward
//!   (min(1% of revoked, cap)) is computed by the node after revocation
//! - Failed challenger: stake forfeited
//! - Expired challenge: half the stake returned, half forfeited

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
///
/// Endorsers receive NO protocol reward (decision 33.8a) — endorsement is a
/// social obligation and the burned BRN is gone regardless of outcome.
#[derive(Clone, Debug)]
pub struct EndorserOutcome {
    /// The endorser's wallet address.
    pub address: WalletAddress,
    /// Amount of BRN permanently burned for the endorsement.
    pub brn_burned: u128,
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
    /// TRST reward for correct staked voters: an equal share of the forfeited
    /// dissenter stakes (decision 33.7d). Burn-backed — the dissenters' BRN
    /// was burned, and this TRST is minted against exactly that burn.
    pub trst_reward: u128,
    /// Penalty: stake forfeited/burned (incorrect voters only).
    pub penalty: u128,
}

/// Process a completed verification and compute rewards/penalties.
///
/// Endorsers get no protocol reward (33.8a). Correct staked verifiers get
/// their stake back plus an equal TRST share of all forfeited dissenter
/// stakes (33.7d). Incorrect verifiers lose their entire stake.
pub fn compute_verification_outcomes(
    wallet: &WalletAddress,
    result: VerificationResult,
    endorsers: &[(WalletAddress, u128)],
    verifiers: &[(WalletAddress, u128, bool)],
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
        .map(|(addr, burned)| EndorserOutcome {
            address: addr.clone(),
            brn_burned: *burned,
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
                    trst_reward: reward_per_correct,
                    penalty: 0,
                }
            } else if *correct {
                // Correct but zero stake (Neither voters) — no reward
                VerifierOutcome {
                    address: addr.clone(),
                    staked: 0,
                    voted_correctly: true,
                    trst_reward: 0,
                    penalty: 0,
                }
            } else {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: *staked,
                    voted_correctly: false,
                    trst_reward: 0,
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
/// If fraud is confirmed, the challenger's stake is returned and the node
/// grants a TRST reward of min(1% of revoked TRST, cap) after revocation
/// (parameter table in IMPLEMENTATION_DECISIONS). If the challenge is
/// rejected, the challenger's stake is forfeited. If it expires, half the
/// stake is returned. Verifier outcomes follow the same reward/penalty
/// logic as regular verification.
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
                    trst_reward: reward_per_correct,
                    penalty: 0,
                }
            } else if *correct {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: 0,
                    voted_correctly: true,
                    trst_reward: 0,
                    penalty: 0,
                }
            } else {
                VerifierOutcome {
                    address: addr.clone(),
                    staked: *staked,
                    voted_correctly: false,
                    trst_reward: 0,
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
            verifier_outcomes,
        },
        ChallengeResult::ChallengeRejected => ChallengeOutcomeEvent {
            challenged_wallet: challenged.clone(),
            challenger: challenger.clone(),
            outcome: ChallengeResult::ChallengeRejected,
            challenger_stake: stake,
            verifier_outcomes,
        },
        ChallengeResult::Expired => ChallengeOutcomeEvent {
            challenged_wallet: challenged.clone(),
            challenger: challenger.clone(),
            outcome: ChallengeResult::Expired,
            challenger_stake: stake,
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
        // 33.8(a): endorsement is a social obligation — no protocol reward,
        // and the burned BRN is permanently gone.
        assert_eq!(outcome.endorsers[0].brn_burned, 1000);
        assert_eq!(outcome.endorsers[1].brn_burned, 2000);
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

        // Total dissenter stakes: 600 + 400 = 1000 (BRN, burned)
        // 2 correct verifiers: each gets a 500 TRST share — burn-backed
        // by the forfeited stakes (33.7d). The stake itself is unlocked
        // separately by the node.
        assert_eq!(outcome.verifiers[0].trst_reward, 500);
        assert_eq!(outcome.verifiers[0].penalty, 0);
        assert_eq!(outcome.verifiers[1].trst_reward, 500);
        assert_eq!(outcome.verifiers[1].penalty, 0);

        // Dissenters lose their stakes
        assert_eq!(outcome.verifiers[2].trst_reward, 0);
        assert_eq!(outcome.verifiers[2].penalty, 600);
        assert_eq!(outcome.verifiers[3].trst_reward, 0);
        assert_eq!(outcome.verifiers[3].penalty, 400);

        // Conservation: minted TRST rewards never exceed the burned stakes.
        let total_rewards: u128 = outcome.verifiers.iter().map(|v| v.trst_reward).sum();
        let total_forfeited: u128 = outcome.verifiers.iter().map(|v| v.penalty).sum();
        assert!(total_rewards <= total_forfeited);
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

        // No dissenters — nothing forfeited, so no TRST reward (stake is
        // simply unlocked by the node).
        assert_eq!(outcome.verifiers[0].trst_reward, 0);
        assert_eq!(outcome.verifiers[1].trst_reward, 0);
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
        assert_eq!(outcome.verifiers[0].trst_reward, 0);
        assert_eq!(outcome.verifiers[0].penalty, 500);
        assert_eq!(outcome.verifiers[1].trst_reward, 0);
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
        // 1 correct verifier gets the whole 1000 as burn-backed TRST
        assert_eq!(outcome.verifiers[0].trst_reward, 1000);
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
        assert_eq!(outcome.challenged_wallet, challenged);
        assert_eq!(outcome.challenger, challenger);
        assert_eq!(outcome.verifier_outcomes.len(), 2);
        assert!(outcome.verifier_outcomes[0].voted_correctly);
        // TRST share funded by the forfeited dissenter stake.
        assert_eq!(outcome.verifier_outcomes[0].trst_reward, 500);
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
        assert!(outcome.verifier_outcomes.is_empty());
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
