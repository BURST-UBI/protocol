# BURST public testnet candidate

Not released yet. The Linux public-testnet software checks are complete within
the operating limits below. Public launch still needs the owner's VPS, verified
bootstrap access and working participant contacts. The requested local creator
backup is verified; it is not an independent disaster-recovery copy.
Test balances have no real value. This is not mainnet software.

## What's included

- `burst-daemon`, the node executable for this Test network.
- `wallet/Burst Wallet_0.1.0_amd64.deb`, the consumer wallet.
- `node-gui/Burst Node_0.1.0_amd64.deb`, the node dashboard.
- Separate component sources and their file manifests in `sources/`.
- `provenance/components.json`, artifact identities and qualification records.
- [Operating limits](OPERATING_LIMITS.md), measured recovery results and unqualified scope.

The candidate targets Linux amd64. The GUI packages require glibc 2.39 or later
and their declared GTK/WebKit dependencies. Windows and macOS are not qualified
by this bundle. Do not bypass package dependency checks.

The initial BRN rate is 1 BRN/hour for verified wallets. Governance can change
that rate. Test uses accelerated governance, including short voting windows and
reduced proposal requirements; these are not mainnet parameters. The faucet is
not part of normal onboarding.

## Check the download

Obtain the release identity and checksum manifest from the publisher's trusted
channel. Checksums downloaded from the same untrusted source do not authenticate
the publisher. From the bundle directory:

```sh
sha256sum -c SHA256SUMS
```

Stop if any check fails. The Test creator public key must be:

```text
948acade65c53fcc5e0d2d001fb1b01edc40b9ea102f1a9ae84d53c92551fd7e
```

This is public information, not a seed. Never share a seed, recovery phrase or
wallet backup with an operator, verifier or support contact.

## Install the apps

After this candidate has been qualified for distribution, install the verified
packages through the system package manager. On a supported Debian-family host:

```sh
sudo apt install './wallet/Burst Wallet_0.1.0_amd64.deb' \
  './node-gui/Burst Node_0.1.0_amd64.deb'
```

Open Burst Wallet from the application menu. Create your wallet and complete its
backup flow before receiving funds or requesting verification. In Settings,
Connection selects the node your wallet uses. The default is
`http://127.0.0.1:7077`, which requires a node on your own computer. Do not send a
seed to that endpoint or expose your local node's RPC port to the internet.

The node dashboard is optional for ordinary wallet use. It does not replace the
daemon or establish a network connection by itself.

## Connect and become verified

The owner will operate release bootstrap infrastructure on a separately
provisioned VPS, not the Azure validation hosts. Its endpoint is not assigned
in this candidate. Planned contacts are `support@brst.cc` and `verify@brst.cc`;
mail delivery has not been verified, so do not advertise them as available yet.
Do not substitute old validation IP addresses or a network using published
fixture keys. Public onboarding remains closed until the endpoint and contact
delivery are confirmed.

For a local node, follow the supplied setup instructions with the released peer
address and a fresh, dedicated data directory. Check the network identity before
connecting your wallet. A reachable RPC endpoint or nonzero peer count does not
by itself mean the node is synchronized and ready to sign.

Each participant owns their wallet. During bootstrap, the creator assesses actual
people and endorses their public addresses. Test funding does not establish
humanity. After bootstrap closes, paid endorsements and selected-human
verification apply. Creating more accounts is not a way to become verified.

## Payments and recovery

Review the recipient and amount before confirming. If a submission times out,
retain its pending action and let the wallet reconcile it. Do not repeatedly
send the same payment or erase its journal. A node restart should not require
creating a new wallet.

Back up the wallet separately from the node ledger. Before moving or changing a
node's storage, stop that node cleanly and preserve its entire data directory,
including signing journals. Never delete data merely to suppress a sync error.
Do not run two daemons against the same directory.

For a wallet created in this app, record all 24 recovery words offline. Restore
them through **Import Mnemonic**, then check that the public address matches your
original wallet before using it. An optional encrypted keystore needs its password;
the password alone is not a backup. If you imported a legacy private key instead,
retain that original recovery material; the app does not turn it into 24 words.

Recovery words and encrypted keystores restore the key, not unsent transactions
or the wallet's pending-action journal. If the old wallet profile is available,
preserve it before changing devices or reinstalling. Do not treat a missing local
pending entry after restore as proof that an earlier payment failed. Confirm its
network outcome before creating a replacement payment.

Restoring a key does not automatically give the new device signing authority.
Review and confirm the wallet's explicit device takeover. Wait for network
confirmation before treating the new device as active. The previous device's
saved grant and drafts then become stale; keep its journals for reconciliation.
This coordinates cooperating wallet copies. It cannot stop someone who holds
the root key from requesting another takeover, so protect recovery material.

An authenticated node snapshot is not a wallet backup. A restored signing node
must retain its signing commitments; a fresh observer is a different role. If
recovery fails, retain the error and logs without posting secrets, and use the
release's support contact once assigned.

## Sign while disconnected

Prepare while connected to capture your account frontier, signing-device grant,
network rules and proof of work. Payment and incoming-receipt preparation is in
the offline payment flow. TRST transfer, batch combination, payment return,
identity and governance reviews have a **Sign offline** action. This includes
endorsements, challenges, appeals, panel votes, proposals, governance votes and
voting delegation. A transfer keeps its selected origin; combining batches keeps
their oldest age. Returning a payment does not undo its original BRN burn.

After preparation, you can disconnect, review the action and choose **Sign and
save**. Signing does not submit it. Keep the app's local profile: recovery words
alone do not preserve that saved action or a proposal's attached content.

Reconnect to submit. If the app closes or the submission response is lost, use
**Review saved transaction** and check confirmation. The wallet retains the
original signed action instead of generating a replacement. Do not discard an
action whose outcome is uncertain. A changed frontier, certified payment epoch,
network rule, proposal phase or signing-device grant can make a saved action stale.
The epoch can change even when your account frontier has not. Offline signing is
not a guarantee of later acceptance.

## Final operator handoff

The release owner is responsible for the following deployment checks. These
need the actual VPS and cannot be established by a local fixture:

1. Verify the bundle checksums and public Test identity on the VPS. Use a fresh
   network directory. Keep the previous data and binaries when upgrading.
2. Keep the creator seed private. Only the creator's signing node receives it;
   observers and participant wallets do not. The local backup is a same-machine
   copy, so keep an independent private backup before relying on it for disaster
   recovery.
3. Make TCP P2P port 17076 reachable and verify another host can join and recover
   the same confirmed state. Keep RPC 7077 and WebSocket 7078 on loopback or a
   private authenticated connection. Do not expose them to make onboarding easy.
4. Test `support@brst.cc` and `verify@brst.cc` delivery. Publish the verified peer
   endpoint, contact method, exact hashes and operating limits together.
5. Create and back up a participant wallet, verify the actual person, and check
   their first confirmed payment and receipt. Stop/restart the VPS service and
   confirm recovery before inviting more participants.

Do not replace these checks with a green peer count. The software evidence
describes controlled tests, not the availability or configuration of this VPS.
Do not publish the candidate while `publicationApproved` is false.

## Software qualification

The bundle's `provenance/components.json` identifies the exact artifacts and
evidence records. The daemon and dashboard are unchanged by the final offline
wallet qualification. No package was rebuilt just to update this handoff.

| Capability | Qualified behavior |
| --- | --- |
| Consumer wallet | Backup/restore, locking, device takeover, payments, receipts, returns, merge and trust-policy flows. |
| Identity and governance | Certified onboarding, challenges, appeals, selected-human votes, proposals, direct/scoped voting and revocation. |
| Offline actions | Reviewed preparation, disconnected signing and durable original-action submission. Recovery checks include lost accepted responses, app restarts and stale-context refusal. |
| Node recovery | Authenticated snapshots, confirmed-state recovery, interrupted catch-up and fresh observers. NAT evidence covers the recorded isolated topologies. |
| Distribution | Named Linux packages, original component sources, exact payload installation and checksums. |

Some journeys combine an original positive run with a retained-state recovery
check. Their original late failures remain in the evidence. This does not claim
that every action was tested at every crash point, or that all whitepaper scale
targets are complete. See the operating limits before inviting participants.

## Qualification limits

The current evidence includes controlled multi-node journeys, process-crash
recovery, authenticated snapshots and isolated NAT/firewall tests. It does not
establish unlimited scale, every physical router, every crash point, or sustained
public-network availability. The operating-limits document records the tested
topologies and measurements. Resource caps used during testing
are not advertised minimum system requirements.

This document does not open the release gate. The candidate manifest remains
marked unreleased until the VPS and operator checks above pass.
