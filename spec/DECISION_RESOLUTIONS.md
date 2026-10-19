# Resolving specification conflicts

## Internal TRST identifiers and release operator (owner decision, 2026-10-19)

The owner accepted deterministic internal identifiers for automatic TRST
fragments, with their original transaction and provenance links retained.
Decision 6.3's transaction-hash identity continues to apply to ordinary
transaction-created holdings. Internal fragments distinguish lifecycle portions;
they do not create funds or require artificial transactions. Preserve the existing
revocation, return and restoration behavior rather than rewriting it to remove
these internal identifiers. This resolves the interpretation, not every lifecycle
implementation claim.

The owner will operate the public testnet on a new, personally provisioned VPS,
not the Azure validation hosts. Do not create or promote an Azure release node.
The selected contact addresses are `support@brst.cc` and `verify@brst.cc`.
Their mailbox/routing setup is not yet verified. Public deployment awaits the
owner's VPS endpoint/access and contact delivery checks. Keep validation peers
out of release bootstrap defaults. No deployment or public publication is
authorized merely by choosing these addresses.

## Offline identity and governance (owner decision, 2026-10-19)

Decision 31.9 includes identity and governance actions, not only payments and
receipts. The owner explicitly confirmed this before public-testnet release.
Prepare the required public context and work while connected, review and sign
without a connection, then submit the exact saved action when connected again.
Offline creation does not promise offline confirmation or waive stale frontier,
epoch, device-grant, panel, deadline or governance-rule checks.

Extend the existing durable action journal and reviewed builders. Identity
endorsements, challenges, appeals, verification votes and verifier participation,
plus governance proposals/content, endorsements, votes, withdrawals and delegation
must retain their operation-specific context. Proposal content must survive
restart alongside its signed commitment. Do not make these operations "offline"
by removing the payment-only guard or accepting arbitrary unsigned block fields.
Acceptance requires positive disconnected signing, later exact submission,
restart/response-loss reconciliation and truthful stale-context handling through
the installed wallet. Existing payment proofs remain valid for their scope.

## Initial public-network rate (owner decision, 2026-10-13)

The owner explicitly selected 1 BRN/hour initially, with governance able to set
the rate to zero later. This confirms decisions 5.3 and 37.1 and supersedes the
whitepaper roadmap's rate-zero launch wording. Initial TRST expiry remains infinite
under decision 6.2. No premine or Live-network test faucet is authorized. Rate zero
remains a required supported economic configuration, not the initial setting.

The owner delegated these choices on 2026-10-10: "do it yourself you understand
the intent behind whitepaper". This record resolves conflicting requirements
without modifying the historical whitepaper or questionnaire. A design decision
here does not mean its implementation is complete.

## C01: preserve merge capacity

Keep 256 explicitly listed inputs. Ordinary serialized blocks have a 1024-byte
limit. Merge blocks may use another 256 times 32 bytes for input hashes, for a
9216-byte limit before the stake-release extension in C07. All messages remain within the 64 KiB body limit. Non-merge
blocks cannot use the merge field as arbitrary data storage.

The input list is needed to reconstruct provenance and preserve the wallet's
choice of expiry groups. Removing it, or shrinking the selected capacity to fit
an inconsistent size estimate, would remove intended functionality.

## C02: work is precomputable

Use the predecessor hash as the work root. For the first block, use a
domain-separated hash of the account address, which is known before choosing a
transaction or timestamp. The signature still authenticates the full block hash.
Work is anti-spam, not transaction authorization or voting power.

Version 2 also makes decision 14.1's Nano-style calculation explicit:
Blake2b with an eight-byte output hashes the little-endian nonce followed by the
32-byte root. Interpret the digest as a little-endian integer and accept values
at or above the threshold. Truncating a 32-byte Blake2b output is not equivalent.
Generation, admission and queue priority share this calculation. The pinned
rsnano `types/src/difficulty.rs` vector supplies an independent regression case
at commit `414dd884c162f78f151b9d912a029bbc9a340866` in `references/rsnano-node`.

This preserves decisions 14.4 and 31.9. Alternative blocks at the same account
position can reuse work; signatures, bounded fork admission and consensus must
still protect the ledger. Changing the target is a consensus change, so new
blocks use version 2. Version-1 ledgers must not be silently reinterpreted.
This development transition requires updated peers and a separate fresh ledger;
no existing data is automatically deleted or migrated.

Block version 3 retains these work rules and adds `target_head` for endorsements.
This signed account-head dependency makes receive/delegation history precede
the endorsement during admission, cementation and catch-up. Wire version 4
rejects older encodings. A zero target head carries no account-head dependency;
it is not proof that an account was absent. Agreed roster epochs and full
lifecycle dependencies remain required; this field is not a substitute for them.

## C03: human voting authority

Do not retain a token-denominated consensus floor or derive voting rights from
BRN/TRST balances. One verified human supplies one unit, possibly delegated.
Eligibility and delegation must come from agreed confirmed state, not provisional
effects or a node's local attendance sample. An offline node cannot redefine who
exists. Online-weight samples may guide networking but must not independently
authorize conflicting finality.

The confirmed-roster/checkpoint design is the integration direction. Its fixed
quorum trades availability for safety during partitions; it does not authorize
an administrator, a monetary fallback, timer-based unlocking or stronger Sybil
resistance claims than the verification system supports. The existing payment
quorum implementation is not signed off by this decision. Roster transitions,
delegation and independent-signer partition tests remain required.

## C04: the endorsement graph is public

Follow decision 33.10. Public endorsement burns, verification outcomes and
provenance must remain independently checkable. Do not claim that hiding a label
in the verifier UI hides an endorser from a node operator. Sensitive evidence
need not be published just because the endorsement relationship is public.
Cryptographic endorser anonymity would require a separately specified design.

## C05: no protocol reward for endorsers

Follow the explicit choice in decision 33.8. Endorsers burn BRN to vouch for
someone; successful verification does not mint an endorser reward. Verifier
compensation and challenge rewards are separate and remain in scope. This avoids
silently introducing a referral subsidy from contradictory whitepaper wording.

## C06: explicit behavior wins over the summary table

Keep decisions 8.5/8.6's no-memo rule and 12.5's immediate cementation after valid
finality. Do not add a five-minute delay from the defaults table. Immediate
cementation does not mean skipping quorum, dependency confirmation or durable
commit. Do not turn the merge-input field into an unofficial memo channel.

## C07: signed returned-stake dependencies

Block version 4 commits `release_sources`, a sorted, distinct list of at most
1024 block hashes. A proof must cover complete signed vote sets that returned
stake to the signer. The node derives amounts from the settled stakes; the
wallet cannot choose a credit amount. Claims are atomic and single-use.
An incomplete set, another account's unavailable credit, an already consumed
release or a block dated before completion is rejected.

The wallet attaches these dependencies to its next signed block. Until then,
admission excludes unacknowledged credits while allowing all other unlocked
BRN to be spent. This preserves decisions 5.5, 5.8 and 5.9 without making
validity depend on packet arrival order. A local timer is not an agreed
deadline proof and cannot unlock spendable credit through this field.

Release hashes add at most 32768 bytes to C01's limits. Even a maximum merge
with a maximum release list remains below the 64 KiB wire bound. The field
cannot carry arbitrary memo data because its entries must match actual stake
settlements. Wire generation 7 rejects older peers. Existing block and BRN
snapshots are not silently migrated; use separate fresh development data.

## Signed payment context, block version 5

Implementation follow-up, 2026-10-17 (unverified, release gated): generation 34
adds an on-demand recovery purpose to authority-only checkpoints. A stale signed
action can remain durably pending on one node while other nodes reject its age.
It cannot be safely discarded from local absence or elapsed time alone. Recovery
therefore uses the existing old-electorate checkpoint quorum and signing fences
to close the epoch with replayed confirmed history. An unchanged roster uses a
distinct recovery commitment and a minimum 60-second interval from the parent
cut. A bounded, epoch-scoped local hint requests discovery; it is not proof of
exclusion and does not authorize a signature. Ordinary timestamp admission stays
unchanged. The wallet releases its reservation only after existing confirmed-slot
or certified-epoch exclusion checks succeed. This is not a periodic global
transaction-production requirement. Runtime recovery and adversarial validation
remain open.

Every ordinary block now includes `payment_epoch` in its owner-signed hash.
The initial value is the configured genesis block hash. Genesis itself uses
zero because its hash cannot contain itself. The node records this context
atomically with genesis, checks it during admission and economic writes, and
requires the same context before reserving a durable ordinary vote. A block
rollback cannot change the installed context. RPC provides it to the wallet;
the wallet refuses a missing, zero or changed context before signing.

This adds 32 bytes to the block format. The existing size limits still apply.
Wire generation 10 rejects older layouts. Existing ledgers are rejected without
migration; testing requires a separate fresh development directory.

Ordinary source-gated nodes still use the genesis context. The gated runtime now
installs certified payment contexts and their agreed equal-human electorate;
local verification or delegation cannot independently change those weights.
Mixed replay accepts
supported signed extensions naming the latest checkpoint hash, applies each
settlement at its exact cut, and rejects extensions predating that cut. This
does not by itself install the outcome or reopen live voting.

The local journal now scopes final promises and fences by payment epoch. The
storage handoff retains old records and permanently closes their context. It
reports locally signed minority choices excluded by the certified cut; they
are not a reason to reject an otherwise valid network decision. Signers still
cannot endorse a cut that omits their own current-epoch final promises.
Recovering an undecided checkpoint journal cannot seal a different installed epoch.
Learning or retransmitting an authenticated decision creates no local signature
and does not require the observer's minority promise to appear in that cut.
Learning fences voting in the still-installed old context, atomically with the
decision journal, without deleting that promise.
Selection and outcome signing both enforce that fence. Each certificate closes
the previous replay context, including a selection with no verifier votes yet.
The runtime serves a stored decision but waits for its installed payment epoch
before starting the next signing round.
Replay also reconstructs and freezes the selection request at that boundary.
A missing beacon leaves the fixed request pending; later endorsements cannot
change it, and no committee gains authority merely from freezing the request.
Certificate authentication, the agreed electorate and live state installation
remain required before any production caller may use the storage handoff.

The gated durable installer now authenticates the next certificate and replays
its cut before committing account/economic state, lifecycle state, roster,
quorum, cutoff and epoch in one transaction. It preserves all cemented positions
and durable confirmation work. Excluded unconfirmed blocks are archived outside
the applied ledger, and old final promises remain intact. Failure aborts the
whole write. Live block admission enforces the installed cutoff as well as the
epoch hash. Sequential selection/fraud-outcome installation and store reopening
have tests, but those tests inject confirmed history. The normal live verification
and rejected-challenge journey now also passes with running-worker publication,
agreed payment electorates, restart and fresh-node catch-up. Authenticated signed
history is archived outside the live ledger so a fresh node need not obtain
ordinary votes from a closed epoch. The installer still refuses a lagging cut
that omits later cemented history. The upheld-challenge journey now also passes
live revocation, a post-outcome payment, restart and fresh-observer recovery.
Roster-only checkpoints now use that same gated path. They update the human
electorate without requesting a committee; the old electorate must certify the
change, including delegation changes that concentrate voting weight. The initial
two-node journey passes through restart and fresh-node catch-up. Bootstrap and
mixed-event regression results are tracked in reference-review iteration 79.
Normal and challenge timeout settlement now have focused and live evidence in
iterations 82 and 84. Iteration 85 adds a separate passing early-vote journey
through activation, deadline settlement and recovery. Re-verification and
adversarial handoff coverage remain open.
The release gate is unchanged.

## C09: rolling Neither-vote history

Decision 15.3 chooses a penalty strictly above 50% Neither votes in a rolling
window. Lifetime totals do not satisfy that choice. The whitepaper also makes
numeric protocol rules governable. Store the threshold and window length in
`ProtocolParams`, and expose both through the existing governance process.

Neither source specifies the window length. Under the owner's delegated design
authority, the initial window is the most recent 100 completed assignments per
verifier. Signed votes and certified timeout votes each count once. A partial
window uses its observed assignment count; there is no hidden minimum sample.
The threshold starts at 5000 basis points and uses an exact strict comparison.
Neither still requires no stake and receives no reward.

A window decrease discards its oldest retained assignments at activation. A
window increase fills with subsequent assignments; it cannot reconstruct
discarded votes. Setting the window to zero clears and disables tracking.
A threshold of 10000 or greater disables new penalties without a constitutional
floor. These changes do not cancel an existing cooldown or rewrite settled
rewards. Completing a new penalty clears that verifier's window, as before.

The rolling order and count are persisted together and validated on restore.
Old snapshots with nonempty lifetime counts lack that order and require replay;
converting them into invented votes or silently clearing them is not allowed.
The parameter encoding and wire generation change. Existing node data is not
rewritten or deleted, and the running apps are not upgraded by this change.

## C08: challenge review and economic correction

Decision 7.4 requires a meta-challenge to dispute a wrong challenge outcome.
Decision 6.15 separately permits ordinary re-verification to restore current
identity and originated TRST. An appeal reviews the earlier judgment; it is not
an endorsement shortcut for a fresh verification attempt.

The signed appeal names the latest certified challenge outcome for its target
and retains its reason. A later independent verification cannot be overwritten
by a stale appeal. The sponsor must be verified and stake the challenge amount.
There is one active challenge or appeal per target, as required by 17.7. The
eligible verifier pool follows 17.4, including previous verifiers. Existing
identity restrictions remain until the previous human electorate certifies the
new judgment. A revoked target cannot sign its own restoration into authority.
Restored BRN accrual resumes at that certified boundary, retaining earned BRN
and recorded pauses rather than granting an unrecorded backdated credit.

An appeal succeeds when the new judgment differs from the reviewed judgment.
Its sponsor then recovers the new stake; otherwise that stake is forfeited.
Fresh verifier stakes and burn-backed rewards follow the existing panel rules.
Historical BRN burns, returned-stake claims and signed balances are not rewound.
Neither source specifies a debt mechanism for previously spent stake returns;
this resolution does not invent one.

Reward corrections follow the original challenge and every appeal of that
judgment. Rewards from judgments inconsistent with the latest result become
non-transferable through their recorded issuance origins. Consistent rewards
can become valid again, subject to expiry and independent identity revocation.
Corrections do not revoke the recipient's unrelated origins or mint a second
copy of an old reward. Reviewing a failed appeal must still correct earlier
wrong rewards in the same history.

The first upheld fraud judgment in that history pays the ordinary bounded
fraud bounty, including when an appeal is the first judgment to find fraud.
Further reversals restore or invalidate that issuance instead of paying another
bounty for the same judgment history. Inactivity reviews never pay a fraud
bounty. This prevents alternating appeals from repeatedly minting against the
same revocation while retaining the incentive to expose a wrongly cleared fraud.

These rules fill in review mechanics left unspecified by 7.4. They do not claim
that appeals are deployed or fully verified. The separate appeal release gate
stays closed until live settlement, recovery and adversarial cases have evidence.

## C10: Emergency participation and verifier balance

Decision 20.11 chooses a shorter emergency vote with a higher quorum and suggests
95%. Quorum means participation by eligible verified humans, not approval among
the people who happened to vote. Use a 95% emergency quorum. Retain the existing
95% approval default as a separate parameter; abstentions count for participation
but not approval. Both thresholds and the initial 24-hour voting period are
governable under decision 20.2. An emergency changing the constitutional threshold
must also satisfy that constitution's existing approval and participation rules.
The emergency route cannot bypass the whitepaper's self-referential protection.

Verified humans can submit emergency proposals through the normal authenticated
entry point. They enter voting immediately and omit support and cooldown. There
is no foundation or representative override. Submission fixes the voting period
and propagation buffer; certified counting uses the then-current thresholds.
Changes to emergency thresholds can themselves be proposed through ordinary
governance, with decision 20.10's ordinary thresholds.

Decision 15.5's verifier minimum defaults to 50 available BRN at the selection
cut. Make this amount governable too. It is not the voting stake: opting in,
verified age, real completed external TRST activity and the separately governed
vote stake retain their existing checks. Zero removes only the balance minimum.

Decision 20.1 sets proposal endorsement burns to 100 BRN. Correct the stored
default of 336 BRN; do not change verification endorsement burns, which remain
336 BRN. These corrections change genesis parameters and wire generation to 29.
They do not authorize deleting a ledger or opening a release gate. Emergency
admission remains behind the full-governance gate, including its compact form.

## Certified governance activation order

Proposal submission fixes its proposal window, voting window and counting
buffer from the rules used to validate that transaction. Changing a duration
applies to new proposals; it cannot move an existing counting cut behind an
already certified checkpoint. This resolves the interaction between concurrent
proposals in decision 20.6 and governable phase durations in the whitepaper.
The sources do not specify this transition rule explicitly.

Before activating new rules, close elapsed proposal windows under the old
qualification threshold. A proposal that missed that threshold stays rejected
if governance later lowers it. This does not freeze every governance parameter
at submission. Ballot quorum and supermajority use the pre-activation rules at
counting; endorsement burns use the rules for their signed transactions.

A ballot receipt binds its activation timestamp as well as its tally. Normal
proposals use the counting cut plus the approved cooldown and propagation
buffer. Emergency proposals omit the cooldown. Later duration changes cannot
move a schedule already certified. Checked arithmetic rejects overflow; zero
cooldown and buffer remain valid and apply activation in the counting certificate.

At a shared cut, settle due verification and challenge panels under the old
rules before deriving the electorate. Count all due ballots under the
pre-activation rules.
Then apply due approved proposals in ascending proposal-hash order. Distinct
activation timestamps retain chronological order. For simultaneous changes to
the same parameter, the last hash wins. This supplies decision 20.9's missing
tie-break without selecting a winner from arrival order or a genesis key.

Then settle pending-transfer expiry under the resulting expiry period. Decision
6.9 applies that period to existing tokens, including previously expired tokens.
An extension at a pending transfer's expiry boundary therefore preserves the
receiver's claim. A transfer returned at an earlier certified cut stays with
the sender even if the extension restores its transferable portfolio balance.
Zero means immediate expiry, not infinite expiry (whitepaper economic spectrum).
Earlier panel deadlines and pending expiry must close before this governance
cut; preparation rejects skipped effects. The governance commitment binds both
panel settlement and pending returns. Wire generation 15 separates this order
from the earlier governance-cut encoding; block format remains v5.

The governance certificate commits the resulting parameter hash. Later signed
blocks must name the installed epoch and those governed rules. The gated
installer publishes the rules with the economic state. Automatic counting,
activation and continuous-traffic finality fencing have native-network evidence
in reference-review iterations 107 and 110–113. Iteration 114 covers parameter
changes during an active normal verification. Partitioned traffic and challenge
rule transitions remain unsigned. This implementation record does not open
either source gate.

### Concurrent constitutional diffs

Decisions 20.6 and 21.3–21.4 allow concurrent, independent patches. They do not
specify how to handle a target removed by an earlier activation. The engineering
resolution is to keep the existing activation order: chronological cut, then
ascending proposal hash within a cut. Patches modify numbered articles in that
order. Two modifications of the same live article apply in order; the latter
text remains. Numeric changes retain decision 20.9's last-activation-wins rule.

Admission checks each patch against the current constitution. If an earlier
activation later removes a required target, the approved patch becomes
`Conflicted`, not `Rejected` or `Activated`. None of its operations apply. Its
votes and missing/already-repealed article number remain in the durable record.
Other approved proposals continue. There is no automatic rewrite, resurrection,
refund or extra voting round. A replacement requires a new proposal. Errors
other than those two target conflicts still reject the staged cut.

Governance-cut commitment v5 binds each activation's conflict outcome and article
number. Wire generation 22 separates these semantics and the new constitution
journal from older nodes. Ordinary builds retain both source gates. Native
wallet authoring and readable review of these outcomes remain separate work.

## Signed credit for expired pending returns

Decision 6.16 returns expired unclaimed TRST to its sender. Decision 6.9 can
later make those tokens transferable again. A certified expiry return now also
leaves a one-time credit receipt under the sender and the original pending
source hash. The return already delivers the tokens and their reputation;
the receipt is not another holding or an outgoing pending amount.

The sender signs an ordinary Receive for the receipt's exact nominal amount.
That block increases the signed balance and consumes the receipt atomically,
without delivering tokens again. A second claim, a changed amount or a
RejectReceive fails. The receipt can be acknowledged while its tokens are
expired or revoked; acknowledgement does not change either condition. Sending
still checks the current portfolio and origin, including partial revocation.
Receipts do not expire and are excluded from pending-token revocation and
restoration, because the returned portfolio already accounts for those effects.

This was introduced in wire generation 16 and pending format 2. The signed block format stays
v5. Opening an older ledger fails without deleting or rewriting it; testing this
generation requires a separate fresh data directory. The account-pending RPC
marks credit receipts so clients can offer receipt acknowledgement without
offering a return-to-sender action. The wallet SDK rejects that action locally.

## Ordinary voting before certified governance activation

An installed checkpoint derives the earliest outstanding activation from its
certified governance state. It stores that timestamp limit with the payment
epoch in the same durable projection. Ordinary soft and final vote reservations
reject signed block timestamps beyond that limit before changing a sequence or
final choice. This check shares the reservation's writer transaction. A caller
without a timestamp cannot bypass an active limit.

Restart authenticates the saved limit against replayed certified history.
An invalid, missing or altered limit is an error, not a request to repair the
ledger. Only another authenticated installation replaces the limit. It does
not erase an earlier promise. Wire generation 17 separates this voting rule
from earlier nodes; pending format 2 and signed block v5 are unchanged.

Confirmed proposal support now supplies a separate counting limit. Cementation
updates that schedule and its epoch-bound limit in the same transaction as the
confirmation height. Admission alone does not publish it. A provisional
withdrawal cannot lift it, and rollback journals cannot rewrite it. Both limits
are checked before ordinary vote reservations. Certified installation replaces
the schedule; startup reconstructs it from the certificate and cemented tail.
Wire generation 18 separates the additional voting rule.

Final support now reserves its deadline before the vote is signed. The same
writer transaction checks the greatest timestamp already promised by that
representative in this epoch. If the earlier promise lies beyond the proposed
deadline, support cannot get that representative's final vote. In the other
order, the saved deadline prevents the later promise. Soft support publishes no
deadline. First paid support reserves the boundary even below the threshold;
the threshold still determines whether the proposal enters voting. When the
threshold is zero, the proposal's own final vote reserves its deadline.

At the counting cut, a supported proposal below threshold receives a certified
rejection, without voting or activation. A supported withdrawal closes as
Withdrawn. This lets the certificate retire old-epoch limits without deleting
final choices. Another certificate containing a terminal proposal also closes
its old-epoch obligation. Terminal receipts do not refund endorsement burns.
The governance-cut commitment is v4 and the wire generation is 19. Existing
timestamp-free final journals cannot silently acquire a new high-water mark.

Wire generation 20 and confirmed-governance index v2 add the submission-time
windows described above. Old indexes or registered proposals without saved
timing cannot silently use today's durations. Live partitions, competing typed
constitutional diffs and other in-flight rule changes still need their recorded
validation. Neither source gate is opened.

## Verification before sign-off

### Governance during an active verification

The selection certificate fixes the eligible pool, selected committee and beacon
context. It does not freeze all numeric protocol rules until the panel ends.
The whitepaper requires transactions after governance activation to use the new
rules. New verifier votes therefore use the active parameter hash and stake
amount. Earlier votes retain their recorded stakes; settlement checks them
against their original signed receipts. Counting uses the rules at the counting
boundary, including the verification threshold and revote limit.

A certified activation fixes the panel's deadline. It records the activation
rules so later timeout changes cannot move that deadline. A panel activated
after a rule change uses the rules at activation, even if its committee was
selected earlier. This supplies the transition detail left implicit in the
whitepaper, without resampling a committee after its beacon is known.

Committee installation checks current rules against the installed certificate.
An arbitrary local parameter edit is not a governance activation. Activation
metadata now includes its rule snapshot and the normal deadline commitment is
v2. Wire generation 21 separates these semantics; old metadata is not silently
upgraded. Reference-review iteration 114 records the passing active-panel
transition, settlement, spending, restart and fresh-observer journey. Challenge
transitions and partition recovery still need their own live evidence.

Each implementation must have scoped regression tests and a running-network
check where applicable. Keep source sections marked partial until their actual
behavior is verified. The verification/challenge release gate stays closed until
the certified lifecycle is integrated, regardless of these resolved choices.
