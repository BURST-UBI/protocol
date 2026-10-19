# Public testnet operating limits

This candidate is not approved for public distribution yet. These are the tested
conditions and known limits, not a mainnet readiness claim. Test balances have
no real value.

## Platform and network

The assembled packages target Linux amd64. The GUI packages require glibc 2.39
or later and their declared GTK/WebKit dependencies. Installation and native
runtime checks on the validation hosts do not qualify Windows, macOS, every
Linux distribution or every desktop compositor.

Use the public creator identity recorded in `identity.json`. The initial BRN
rate is 1 BRN/hour for verified wallets. Governance can change it. Test uses
accelerated governance rules; do not interpret its timing, fees or quorum as
mainnet settings. Published-key staging networks are disposable and must not be
used as the public network.

Public bootstrap endpoints and the initial participant-verification contact
must be supplied before launch. Keep RPC and WebSocket interfaces private.
Opening the P2P port does not require opening either administrative interface.

## What the recovery evidence covers

The table retains measurements from earlier v6 packages. Source-impact review
and the affected v7 checks are recorded in `spec/COMPLETION_LEDGER.md`. They allow
reuse within the named scope, not relabeling an older measurement as a new run.
The final offline wallet checks do not change daemon transport or recovery code.

The current v7 packages pass public daemon identity/restart checks, installed
wallet consumer flows, dashboard navigation/RPC checks and exact-action recovery
after a lost reply and locked restart. Fresh certified onboarding completed its
positive actions; a later readback confirmed all three nodes and lock behavior
after the original test expected the wrong lock-error wording. Snapshot export,
authentication, installation and crash/reconnect checks also passed their stages;
the final payment passed separately after fixing its test preflight. These are
scoped results, not uninterrupted timing claims or permission to launch.

| Check | Observed result | Scope |
| --- | --- | --- |
| Empty observer joins the retained lifecycle/governance network | 159,994 ms to the pinned certified epoch, confirmed heads and signing readiness | Current staging daemon; three sources and one observer sharing a one-CPU job limit |
| Same observer restarts after SIGKILL | 40,510 ms to the same certified state and readiness | Same ledger and fixture; lifecycle, payments and governance checked again |
| Authenticated snapshot recovery | Altered archive rejected; installed state and a later payment preserved across restart | Current staging daemon on loopback and two separate Azure hosts |
| Uncertain payment recovery | Exact draft survives node SIGKILL and wallet restart; duplicate signing blocked, old epoch certifiably closed, separately reviewed payment confirmed on all three nodes | Current staging daemon and packaged wallet; dropped submission, not every mid-commit crash or action type |
| Public artifact identity | Public daemon identity, initial rate, authority/funding gates and restart pass on both hosts; installed GUI payloads match the packages | No fake-human endorsements under the public identity |
| NAT and firewall handling | Retained isolated outbound/double-NAT, reconnect, forwarding and UPnP-emulator checks | Unchanged transport inputs; not every physical router or a distributed quorum behind NAT |

The empty-observer checks retained their 180-second test deadline. That deadline
and the measured times describe this fixture; they are not a promise that any
history or network connection will synchronize within three minutes.

Runtime validation jobs were capped at one CPU equivalent, 3 GiB RAM, zero swap
and 20 minutes. Multi-node loopback jobs shared that cap. Independent-host jobs
had a cap on each host. These caps are not measured minimum requirements for a
public node, a maximum memory guarantee for arbitrary workloads or a throughput
benchmark.

The optimized node integration check also recovered a 130-checkpoint history in
70.273 seconds under its unchanged 90-second deadline. That was a controlled-time
test executable, not a capacity benchmark for the distributed daemon. It supports
the recovery implementation; it does not establish unlimited history growth.

## Limits that remain

The retention update has source-level pressure/recovery tests. Exact daemon
`a10053c7...` additionally passes signed admission and restart checks; that smoke
check is not a release-binary load test. The update shares 65,536 block slots across ingress,
authentication, unchecked dependencies and released recovery batches. Unchecked
storage leaves 4,096 slots for incoming dependencies. Already reserved recovery
blocks retain their slots when moved back to the queue.

An actual two-node test filled the 4,096-entry ingress queue with a repeated
missing-parent block. Overflow was rejected; after startup, the parent and child
reached both nodes and survived restart without changing balances. This does not
measure sustained load, 65,536 distinct missing dependencies or worst-case memory.
Rebroadcast and governance-content caches have their own limits; the shared slot
counter is not a cap on every allocation in the process.

No sustained public-load capacity, maximum participant count or transactions-per-
second rate is claimed. Whole-state engine projections, retained authority/history
and storage growth remain scaling work. Current source qualification does not
establish indefinite operation at arbitrary scale.

Controlled process crashes do not cover every power-loss, disk-failure, action
type or commit point. Source archives pass file checksums and offline Cargo
metadata checks; those checks are not reproducible-build verification. A matching
checksum authenticates bytes only when its expected value came from a trusted
release channel.

Keep these limitations in the release notes. Do not present experimental Test
qualification as permission to enable Live or to entrust real-value funds.

## If the node cannot keep up

Keep the node's logs, data directory and signing journals. Check disk space,
memory pressure, selected network and peer connectivity. A peer connection alone
does not prove signing readiness. Do not bypass a recovery guard or delete data
to make a status indicator turn green.

Retain uncertain wallet actions until the network confirms or certifiably excludes
them. Do not sign replacements merely because an RPC request timed out. Stop the
owning node cleanly before moving its storage, and never run two instances against
the same directory. An observer can use a new empty directory; a signing node
must preserve its commitments and follow the recovery procedure.

When reporting a problem, include the artifact hash, network, operation, error and
whether an action is pending. Do not send seeds, recovery phrases or wallet backups.
