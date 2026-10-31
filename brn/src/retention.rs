//! Dependency identities for retention traversal; never deletion authority.
use burst_types::{BlockHash, Timestamp, WalletAddress};
use std::ops::ControlFlow;

/// Temporal boundaries require the original rate/interval arithmetic. They are
/// not permission to replace a history prefix with an unchecked cached balance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference<'a> {
    Account(&'a WalletAddress),
    Block(BlockHash),
    Outcome(BlockHash),
    Stake(crate::StakeId),
    Boundary(Timestamp),
}

impl crate::Stake {
    /// Include resolved stakes too: settlement/anti-replay dependencies must be
    /// established before a coordinator can consider retiring their records.
    pub fn visit_retention_references<'a, B>(
        &'a self,
        mut visit: impl FnMut(Reference<'a>) -> ControlFlow<B>,
    ) -> ControlFlow<B> {
        visit(Reference::Stake(self.id))?;
        visit(Reference::Account(&self.staker))?;
        let target = match &self.kind {
            crate::StakeKind::Verification { target_wallet }
            | crate::StakeKind::Challenge { target_wallet } => target_wallet,
        };
        visit(Reference::Account(target))?;
        visit(Reference::Boundary(self.created_at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stake_roots_preserve_both_kinds_and_resolved_state() {
        let owner = WalletAddress::new("brst_owner");
        let target = WalletAddress::new("brst_target");
        for resolved in [false, true] {
            for kind in [
                crate::StakeKind::Verification {
                    target_wallet: target.clone(),
                },
                crate::StakeKind::Challenge {
                    target_wallet: target.clone(),
                },
            ] {
                let stake = crate::Stake {
                    id: 7,
                    staker: owner.clone(),
                    amount: 8,
                    kind,
                    created_at: Timestamp::new(9),
                    resolved,
                };
                let mut refs = Vec::new();
                assert_eq!(
                    stake.visit_retention_references(|r| {
                        refs.push(r);
                        ControlFlow::<()>::Continue(())
                    }),
                    ControlFlow::Continue(())
                );
                assert_eq!(
                    refs,
                    [
                        Reference::Stake(7),
                        Reference::Account(&owner),
                        Reference::Account(&target),
                        Reference::Boundary(Timestamp::new(9))
                    ]
                );
                for stop in 1..=refs.len() {
                    let mut calls = 0;
                    assert_eq!(
                        stake.visit_retention_references(|_| {
                            calls += 1;
                            if calls == stop {
                                ControlFlow::Break(())
                            } else {
                                ControlFlow::Continue(())
                            }
                        }),
                        ControlFlow::Break(())
                    );
                    assert_eq!(calls, stop);
                }
            }
        }
    }
}
