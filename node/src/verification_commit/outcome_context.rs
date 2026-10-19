use super::*;

impl Settlement {
    /// The complete signed vote set, not packet arrival, determines the outcome
    /// time and reward source. Distributed admission still requires an agreed cut.
    pub(super) fn from_events(
        batch: &impl Ledger,
        target: &WalletAddress,
        events: &[VerificationEvent],
        network: burst_types::NetworkId,
    ) -> Result<Option<Self>, String> {
        let mut context = None;
        for event in events {
            let (wallet, voters, challenger) = match event {
                VerificationEvent::VerificationComplete {
                    wallet, outcomes, ..
                } => (wallet, outcomes.verifiers.as_slice(), None),
                VerificationEvent::ChallengeResolved { wallet, outcome } => {
                    if outcome.outcome == ChallengeResult::Expired {
                        return Err("challenge expiry requires its agreed deadline".into());
                    }
                    (wallet, outcome.verifier_outcomes.as_slice(), Some(outcome))
                }
                _ => continue,
            };
            if wallet != target || context.is_some() || voters.is_empty() {
                return Err("invalid or duplicate verification outcome".into());
            }
            let mut sources = Vec::with_capacity(voters.len() + usize::from(challenger.is_some()));
            let mut absent = HashSet::new();
            for voter in voters {
                if voter.selection_context.is_none()
                    && crate::verification_deadline::is_recorded_default(
                        batch,
                        target,
                        &voter.address,
                        challenger.and_then(|outcome| outcome.challenge_context),
                    )?
                {
                    if voter.staked != 0 || voter.penalty != 0 || voter.trst_reward != 0 {
                        return Err("certified default has economic effects".into());
                    }
                    absent.insert(voter.address.clone());
                    continue;
                }
                sources.push(stake_source(
                    batch,
                    target,
                    &voter.address,
                    voter.staked,
                    false,
                    voter.selection_context,
                )?);
            }
            if let Some(challenger) = challenger {
                sources.push(stake_source(
                    batch,
                    target,
                    &challenger.challenger,
                    challenger.challenger_stake,
                    true,
                    challenger.challenge_context,
                )?);
            }
            let source = sources
                .iter()
                .max_by_key(|block| (block.timestamp.as_secs(), *block.hash.as_bytes()))
                .ok_or("verification outcome has no signed source")?;
            context = Some(Self {
                network,
                hash: source.hash,
                target: target.clone(),
                timestamp: source.timestamp,
                sources: sources.iter().map(|block| block.hash).collect(),
                authority: if absent.is_empty() {
                    SettlementAuthority::SignedVotes
                } else {
                    SettlementAuthority::SignedVotesWithDefaults { absent }
                },
            });
        }
        Ok(context)
    }
}

pub(super) fn stake_source(
    batch: &impl Ledger,
    target: &WalletAddress,
    staker: &WalletAddress,
    amount: u128,
    challenge: bool,
    attempt: Option<BlockHash>,
) -> Result<StateBlock, String> {
    let attempt = attempt
        .filter(|hash| !hash.is_zero())
        .ok_or("outcome stake attempt is missing")?;
    let receipt = read_stake(batch, &stake_key(target, staker, challenge, attempt))?
        .ok_or("outcome stake receipt is missing")?;
    if receipt.amount != amount || receipt.settled_by.is_some() {
        return Err("outcome stake mismatch or duplicate".into());
    }
    let bytes = batch
        .get_block(&receipt.block)
        .map_err(|error| error.to_string())?;
    let block: StateBlock = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(&bytes)
        .map_err(|error| error.to_string())?;
    let expected_kind = if challenge {
        BlockType::Challenge
    } else {
        BlockType::VerificationVote
    };
    let observed_attempt = if challenge {
        block.hash
    } else {
        BlockHash::new(*block.origin.as_bytes())
    };
    if block.block_type != expected_kind
        || block.hash != receipt.block
        || block.compute_hash() != receipt.block
        || block.account != *staker
        || observed_attempt != attempt
        || crate::ledger_bridge::extract_receiver_from_link(&block.link).as_ref() != Some(target)
    {
        return Err("outcome stake source does not match signed attempt".into());
    }
    Ok(block)
}
