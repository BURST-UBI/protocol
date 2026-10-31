# Bootstrap permissions

Ordinary enrollment relies on people who are already verified. A new network
has too few of them to start that process. BURST therefore gives its creator
temporary permissions to help enroll the first participants. This starting
period is called bootstrap.

It is a real exception to equal authority. The creator can also veto approved
governance proposals during this period. The limit matters: once bootstrap ends,
the creator keeps an ordinary account without special verification or governance
authority, and a later fall in population cannot bring those powers back.

## When authority ends

Bootstrap ends when the verified-account count reaches the configured exit
threshold, capped at 50. The creator counts toward that total. A threshold of zero
or one ends bootstrap at genesis.

The phase is stored with the ledger state. Once it is complete, a later drop in
verified accounts or an increase in the threshold cannot reopen it. Restart and
recovery must preserve that phase. Missing or corrupt phase records are errors;
they do not grant bootstrap permissions.

See [`BootstrapPhase`](https://github.com/BURST-UBI/protocol/blob/main/verification/src/bootstrap.rs) and its
[persistent state](https://github.com/BURST-UBI/protocol/blob/main/node/src/bootstrap_phase.rs).

## Governance veto

An approved proposal activates through its certified boundary without a separate
creator approval signature. During active bootstrap, the verified creator can
submit a signed `GovernanceVeto` block naming the proposal.

A veto is valid only from approval time up to, but not including, the scheduled
activation time. It changes that proposal to the terminal `Vetoed` phase. Ending
bootstrap does not reverse an existing veto; changing the rejected proposal
requires a new proposal.

The creator cannot enact a proposal alone, extend bootstrap beyond the cap, or
veto after bootstrap ends. Ordinary consensus quorum is still required for
progress. A creator who also supplies quorum-critical representative weight can
affect availability for the same reason any quorum-critical representative can.

See [veto authorization](https://github.com/BURST-UBI/protocol/blob/main/node/src/governance_commit.rs),
[veto timing](https://github.com/BURST-UBI/protocol/blob/main/governance/src/engine.rs) and
[certified activation](https://github.com/BURST-UBI/protocol/blob/main/node/src/confirmed_replay/governance.rs).

## Bootstrap verification

Bootstrap provides a creator verification path that skips ordinary endorsement
and verifier-panel requirements. The node must enforce the persisted bootstrap
phase as well as the signer and account rules. Calling a verification-engine
helper alone does not establish valid network authorization.

This exception concerns verification authority. It does not imply an exemption
from BRN accounting or solve initial funding. See the
[network launch requirements](../node/NETWORK_LAUNCH.md).

## Key compromise

During bootstrap, possession of the creator key permits its remaining bootstrap
actions, subject to admission rules. This includes genesis verification,
governance vetoes and epoch markers. Epoch admission checks the persisted phase
and rejects creator markers once that authority ends. An epoch marker does not
install software or replace consensus rules.

After exit, compromising the key exposes the creator account's ordinary
permissions, not a continuing bootstrap override. The public genesis identity
remains a network trust anchor; that role does not restore private-key authority.

## Availability

The node rejects governance actions when certified activation is disabled. It
also rejects account-authored `GovernanceActivation` blocks: activation belongs
to the certified transition. The presence of these mechanisms in source code
does not mean they are enabled on every network or released binary.
