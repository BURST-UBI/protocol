# BURST testnet setup and recovery

Public distribution is not approved yet. Follow the
[testnet release checklist](../spec/TESTNET_PILOT.md) for current qualification.
This guide describes the setup contract; it does not provide a released binary
or an active public bootstrap address. Old validation IP addresses and known-key
fixture builds must not be used for a shared public network.

## Before installing

Use a qualified release bundle for your operating system. Its handoff must name
the artifact hashes, network's creator public key, bootstrap peers and supported
operating limits. Obtain that information through the trusted release channel.
Checksums detect changed downloads; a checksum supplied by the same untrusted
download server does not authenticate the publisher.

The current Linux GUI packages require glibc 2.39. A package built or installed
on one platform does not establish support for another. Do not bypass dependency
checks or assume Windows or macOS qualification from Linux results.

From the downloaded bundle directory, verify all listed files before installing:

```sh
sha256sum -c SHA256SUMS
```

Do not pipe an unverified download into a privileged shell. The wallet backup is
separate from the node ledger; back up the wallet through its backup flow and
keep the backup private before using it.

## Network identity and roles

All nodes must use the same creator public identity, pinned into the Test build
with `private-testnet` and `BURST_TEST_GENESIS_PUBLIC_KEY`. `--network test` alone
does not choose or authenticate a new creator identity. Existing published-key
fixtures are disposable test networks, not release candidates.

Only the creator's node receives `BURST_GENESIS_SEED_FILE`. Generate that secret
on the creator's device, outside the repository, and back it up privately before
launch. Never include it in a release bundle, config example, log or chat. See
[the release checklist](../spec/TESTNET_PILOT.md) for creator identity instructions.

A bootstrap peer supplies connectivity; it does not gain genesis authority by
being listed as a peer. Enabling a faucet does not create genesis authority
either. The shared testnet starts at 1 BRN/hour for verified wallets. Normal
accrual, endorsement burns and transfers must work without a faucet. Test-only
funding tools are opt-in and are not a substitute for that flow.

Test also uses accelerated governance rules: proposals have no BRN fee or
endorsement minimum, the quorum is 0.01%, and voting phases last seconds rather
than production-length periods. Verification has no minimum-age gate on Test.
These are the current `ProtocolParams::testnet_defaults`, not mainnet settings.
The release handoff must disclose these rules; test balances have no real value.

## Verify the first participants

The whitepaper's genesis phase starts with the creator as the only verified
wallet. Each participant creates and backs up their own wallet, then shares
their public address. Never collect their seed, recovery phrase or backup.
The creator must assess the actual person before endorsing that address.
Generating accounts or giving them test funds does not establish humanity.

While identity bootstrap is active, the creator's local RPC accepts
`genesis_endorse` with an `account` field containing the participant's address.
This signs an endorsement under the creator's identity and directly verifies
the target when confirmed. It is not a read-only check. The endorsement still
uses the configured BRN burn amount; authority does not imply unlimited funds.
Keep this RPC private and never send the creator seed in an RPC request.

Process one participant at a time. Retain the returned `block_hash` and check
confirmation and `verified` before moving on. If the request times out, inspect
the creator's history and target's verification state before trying again.
Do not interpret an HTTP response alone as successful verification.

The current default bootstrap exit threshold is 50 verified wallets, including
the creator. Once bootstrap has closed, losing verified wallets does not reopen
the creator's special authority. Subsequent participants use paid endorsements
and selected-human verification. Do not lower a threshold, inject fixture
identities or copy signing keys to make onboarding appear complete.

The release handoff must identify who performs these initial human checks and
how participants reach them. Public launch remains blocked until that handoff
and the creator's private backup are ready. The validation fixtures are not the
public network's initial participants.

## Start an observer node

Replace the peer placeholder below with the endpoint in the qualified release
handoff. Use a fresh, dedicated directory for this network. Do not point a new
identity at another network's ledger or erase that ledger to suppress an error.

```sh
./burst-daemon --network test \
  --data-dir ./burst-testnet-data \
  --bootstrap-peers '<release-peer>:17076' \
  --rpc=true --rpc-bind 127.0.0.1 --rpc-port 7077 \
  --websocket=true --websocket-bind 127.0.0.1 --websocket-port 7078 \
  --no-representative node run
```

This node follows the ledger without signing consensus votes. Voting requires
the appropriate verified-human authority and a private representative key; it
is not enabled by adding peers or giving the node more tokens. Do not copy the
creator's key to participant nodes to make them vote.

Check inherited `BURST_*` environment settings before launch. Do not enable
`BURST_ENABLE_FAUCET` or `enable_faucet` in a shared deployment unless the release
handoff explicitly calls for a restricted test-funding service.

Keep RPC and WebSocket listeners on loopback. They are not P2P ports. For remote
wallet access, use the release's authenticated access setup or a private tunnel;
do not expose ports 7077 or 7078 directly to the internet. A local wallet can use
`http://127.0.0.1:7077`. Confirm the wallet displays Test before signing anything.

## Connectivity and health

An observer behind NAT can make outbound connections to reachable peers. Inbound
reachability depends on the router and firewall; UPnP is not guaranteed. A public
peer needs its configured TCP P2P port reachable. A cloud security rule or port
forward should expose the P2P port only, not the administrative RPC surface.
An advertised public address does not itself open a firewall or prove reachability.

Query the local node:

```sh
curl --fail --silent --show-error --max-time 5 \
  http://127.0.0.1:7077 -H 'Content-Type: application/json' \
  -d '{"action":"node_info"}'
curl --fail --silent --show-error --max-time 5 \
  http://127.0.0.1:7077 -H 'Content-Type: application/json' \
  -d '{"action":"peers"}'
```

An RPC response or nonzero peer count does not prove synchronization or signing
readiness. Check recovery status and compare confirmed state with a trusted peer.
Keep pending wallet actions until the network confirms or certifiably excludes
them; do not repeatedly resubmit a payment because a response was lost.

## Recovery without destroying state

For an empty ledger, isolation or startup refusal:

1. Check the selected network, creator public identity, data-directory path and
   startup error. A wrong identity is a configuration problem, not permission to
   reset the ledger.
2. Check bootstrap reachability and the P2P port. Keep RPC private while doing so.
3. If recovery is in progress, retain its logs and let it finish. Do not run a
   second daemon against the same data directory.
4. Before changing storage, stop the owning node cleanly and preserve its entire
   data directory, including signing journals. Do not restore only balances or
   `data.mdb` from an unrelated backup, or discard journals to bypass a refusal.

Use the qualified release's authenticated snapshot procedure when supplied. The
CLI's `snapshot verify` command authenticates an archive; it does not install it
or back up the wallet. Obtain the expected epoch and original genesis parameters
independently, rather than trusting claims inside a downloaded archive.

For a deliberate fresh observer, choose a different empty directory and leave
the old directory intact. A signing node needs its dedicated recovery procedure;
starting it with discarded promises can violate its prior commitments. There is
no blanket delete-and-resync instruction for signing nodes.

Final artifact installation, public endpoints and supported recovery limits
remain release gates in P4/P5. Do not infer readiness from this guide alone.
