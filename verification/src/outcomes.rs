//! Stake settlement and rewards for verification and challenge outcomes.
//!
//! Endorsement burns are permanent and earn no reward. Winning staked verifiers
//! share forfeited dissenting stakes using integer division. The node applies
//! these results and computes any revocation-backed challenger reward.

use burst_types::{BlockHash, WalletAddress};

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
    /// The settled vote passed the verification threshold.
    Verified,
    /// The settled vote did not establish verification.
    Failed,
}

/// Outcome for a single endorser in a verification round.
///
/// The burn is permanent regardless of the outcome; there is no endorser reward.
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
    /// Selection context attached by the orchestrator after calculation.
    /// Absent for synthetic timeout votes and legacy unbound state.
    pub selection_context: Option<BlockHash>,
    /// The verifier's wallet address.
    pub address: WalletAddress,
    /// Amount of BRN staked for this verification.
    pub staked: u128,
    /// Whether the caller classified this vote as agreeing with the settled result.
    /// This is not necessarily a simple majority or proof of factual correctness.
    pub voted_correctly: bool,
    /// Integer share of forfeited dissenting stakes for a winning staked voter.
    pub trst_reward: u128,
    /// Penalty: stake forfeited/burned (incorrect voters only).
    pub penalty: u128,
}

/// Process a completed verification and compute rewards/penalties.
///
/// Endorsers receive no reward. Winning staked verifiers share forfeited stakes;
/// dissenting verifiers lose their stake. The caller supplies the settled judgment.
pub fn compute_verification_outcomes(
    wallet: &WalletAddress,
    result: VerificationResult,
    endorsers: &[(WalletAddress, u128)],
    verifiers: &[(WalletAddress, u128, bool)],
) -> Result<VerificationOutcomeEvent, crate::VerificationError> {
    Ok(VerificationOutcomeEvent {
        wallet: wallet.clone(),
        result,
        endorsers: endorsers
            .iter()
            .map(|(address, burned)| EndorserOutcome {
                address: address.clone(),
                brn_burned: *burned,
            })
            .collect(),
        verifiers: verifier_outcomes(verifiers)?,
    })
}

fn verifier_outcomes(
    verifiers: &[(WalletAddress, u128, bool)],
) -> Result<Vec<VerifierOutcome>, crate::VerificationError> {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0u128;
    for (address, stake, correct) in verifiers {
        if !seen.insert(address) {
            return Err(crate::VerificationError::Other(
                "duplicate verifier settlement".into(),
            ));
        }
        if !correct {
            total = total.checked_add(*stake).ok_or_else(|| {
                crate::VerificationError::Other("verifier reward pool overflow".into())
            })?;
        }
    }
    let count = verifiers
        .iter()
        .filter(|(_, stake, correct)| *correct && *stake > 0)
        .count() as u128;
    let share = total.checked_div(count).unwrap_or(0);
    Ok(verifiers
        .iter()
        .map(|(address, stake, correct)| VerifierOutcome {
            selection_context: None,
            address: address.clone(),
            staked: *stake,
            voted_correctly: *correct,
            trst_reward: if *correct && *stake > 0 { share } else { 0 },
            penalty: if *correct { 0 } else { *stake },
        })
        .collect())
}

/// Outcome of a completed challenge.
#[derive(Clone, Debug)]
pub struct ChallengeOutcomeEvent {
    /// Present only when this panel reviewed an earlier challenge judgment.
    pub appeal: Option<crate::challenge::AppealContext>,
    /// Signed challenge block that locked the challenger's stake.
    pub challenge_context: Option<BlockHash>,
    /// The wallet that was challenged.
    pub challenged_wallet: WalletAddress,
    /// The wallet that submitted the challenge.
    pub challenger: WalletAddress,
    /// The panel's judgment of the target. For an appeal, compare it with
    /// `appeal.previously_legitimate`; this field alone does not say whether
    /// the appeal succeeded or authorize ordinary challenge settlement.
    pub outcome: ChallengeResult,
    /// The BRN stake the challenger put up.
    pub challenger_stake: u128,
    /// Outcomes for each verifier in the challenge vote.
    pub verifier_outcomes: Vec<VerifierOutcome>,
}

/// The result of a challenge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChallengeResult {
    /// The challenge was upheld. Also used for inactivity; the challenge reason
    /// determines revocation versus deactivation, and appeals settle separately.
    FraudConfirmed,
    /// The panel judged the target legitimate. An appeal compares this with
    /// the earlier judgment before deciding its effect.
    ChallengeRejected,
    /// Legacy admission-window expiry. Certified panel deadlines instead fill
    /// missing votes with Neither before tallying.
    Expired,
}

/// Compute the outcome of a challenge.
///
/// Records the supplied judgment and challenger stake, and computes verifier
/// rewards and penalties. The node separately applies challenger settlement and
/// any revocation-backed reward. This function does not check challenge eligibility,
/// debit balances or decide whether an appeal reverses its earlier judgment.
pub fn compute_challenge_outcome(
    challenged: &WalletAddress,
    challenger: &WalletAddress,
    outcome: ChallengeResult,
    stake: u128,
    verifiers: &[(WalletAddress, u128, bool)],
) -> Result<ChallengeOutcomeEvent, crate::VerificationError> {
    Ok(ChallengeOutcomeEvent {
        appeal: None,
        challenge_context: None,
        challenged_wallet: challenged.clone(),
        challenger: challenger.clone(),
        outcome,
        challenger_stake: stake,
        verifier_outcomes: verifier_outcomes(verifiers)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflowing_or_duplicate_stakes_cannot_create_a_reward_pool() {
        for voters in [
            vec![
                (test_address(1), u128::MAX, false),
                (test_address(2), 1, false),
            ],
            vec![(test_address(1), 1, true), (test_address(1), 1, false)],
        ] {
            assert!(compute_verification_outcomes(
                &test_address(9),
                VerificationResult::Verified,
                &[],
                &voters
            )
            .is_err());
            assert!(compute_challenge_outcome(
                &test_address(9),
                &test_address(8),
                ChallengeResult::FraudConfirmed,
                100,
                &voters
            )
            .is_err());
        }
    }

    fn test_address(n: u8) -> WalletAddress {
        WalletAddress::new(format!("brst_{:0>60}", n))
    }

    #[test]
    fn verified_endorsers_receive_no_protocol_reward() {
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
        )
        .unwrap();

        assert_eq!(outcome.result, VerificationResult::Verified);
        assert_eq!(outcome.endorsers.len(), 2);
        // Outcomes retain each burn amount without adding an endorser reward.
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
        )
        .unwrap();

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
        )
        .unwrap();

        // Split 1,000 forfeited units between two staked winning voters.
        // Actual burns, reward minting and stake release belong to the node.
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
        )
        .unwrap();

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
        )
        .unwrap();

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
        )
        .unwrap();

        // Total dissenter: 300 + 400 + 300 = 1000
        // 1 correct verifier gets the whole 1000 as burn-backed TRST
        assert_eq!(outcome.verifiers[0].trst_reward, 1000);
        assert_eq!(outcome.verifiers[0].penalty, 0);
    }

    #[test]
    fn empty_verifiers_and_endorsers() {
        let wallet = test_address(1);
        let outcome =
            compute_verification_outcomes(&wallet, VerificationResult::Verified, &[], &[]).unwrap();

        assert_eq!(outcome.wallet, wallet);
        assert_eq!(outcome.result, VerificationResult::Verified);
        assert!(outcome.endorsers.is_empty());
        assert!(outcome.verifiers.is_empty());
    }

    #[test]
    fn verification_outcome_preserves_wallet() {
        let wallet = test_address(42);
        let outcome =
            compute_verification_outcomes(&wallet, VerificationResult::Failed, &[], &[]).unwrap();
        assert_eq!(outcome.wallet, wallet);
    }

    #[test]
    fn fraud_outcome_records_challenger_stake_and_verifier_rewards() {
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
        )
        .unwrap();

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
    fn rejected_challenge_outcome_records_stake_without_verifiers() {
        let challenged = test_address(1);
        let challenger = test_address(2);
        let stake = 1000u128;

        let outcome = compute_challenge_outcome(
            &challenged,
            &challenger,
            ChallengeResult::ChallengeRejected,
            stake,
            &[],
        )
        .unwrap();

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
        )
        .unwrap();

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
        )
        .unwrap();

        assert_eq!(outcome.challenged_wallet, challenged);
        assert_eq!(outcome.challenger, challenger);
    }
}
