# Privacy and network behavior

BURST is network software. Its wallet, dashboard and protocol node communicate
with different endpoints. This page describes the current source defaults. Configuration and older release
versions may differ.

## Desktop wallet and dashboard

The desktop applications default to a node at `http://127.0.0.1:7077`. A user
can choose another node in the application's connection settings. Requests to a
remote node disclose the connecting IP address and the requested information,
such as an account address, transaction, balance, verification state or
governance query. The remote operator controls that node's logs and retention.
Use a node operator you trust. Plain HTTP to a remote endpoint does not encrypt
the connection.

The wallet creates and imports signing keys locally. Ordinary signed transaction
submissions contain signatures and public transaction data, rather than the
wallet's private seed. Recovery phrases and exported backup files grant access
to the wallet; sharing one is a separate disclosure of that access. The wallet
stores local profile, signing-device and offline-payment state. Backup files are
saved to the location chosen by the user.

The dashboard's telemetry view reads the configured node's operational state.
Its local log and configuration features access files on the computer running
the dashboard. They do not read files from a remote node merely because its RPC
URL was selected.

## Group services and browser requests

Loading a group directory contacts its host but does not query every listed
membership endpoint. Evaluating a payment's group policy can disclose original
creator addresses to the selected services and to registered groups reached by an
explicitly enabled trust path. The responses are membership judgments, not
private or anonymous proofs. Services can retain request logs even though the
client does not reuse membership responses for later evaluations.

The group client rejects redirects. The shared browser transport requests CORS,
omits credentials and uses no-store cache mode. These request settings do not
hide the connecting IP address, prevent server logging or encrypt a plain HTTP
connection. Browser failures can also reflect CORS or redirect rejection, so the
wallet does not treat every failed fetch as permission to use an outage fallback.
See [group service contracts](../node/GROUP_SERVICES.md).

Desktop shell navigation restrictions apply to pages loaded in the application
window. They are separate from the configured RPC URL and group endpoints; an
allowed application page can still make its configured network requests.

## Protocol node

Running a node enables protocol communication. Connected peers can observe its
network address, handshake and node information, and protocol messages. Signed
blocks, account addresses, payment information and governance actions that enter
the ledger are replicated to other nodes. They should not be treated as private
or as information that can be erased from every peer by deleting a local copy.

The current node also uses infrastructure services:

- Clock-offset synchronization contacts `pool.ntp.org` over UDP at startup and
  periodically. The current implementation does not expose a switch for this
  auxiliary NTP request.
- Authenticated time sources default to Cloudflare (`time.cloudflare.com`),
  Netnod (`nts.netnod.se`), SIDN (`ntppool1.time.nl`) and Canonical
  (`1.ntp.ubuntu.com`). Operators can configure independent NTS sources.
- The default public-randomness relay is `https://api.drand.sh`. Operators can
  configure validated drand relay URLs.
- Peer discovery and reachout exchange network information. UPnP is enabled by
  default and can request port mappings from a local router; `--disable-upnp`
  disables that behavior. It does not disable time synchronization.

These endpoints and the DNS services used to resolve them can observe the
connecting network address and request metadata. Their operators apply their
own privacy and retention practices. The project does not control their logs.
Consult the [NTP Pool](https://www.ntppool.org/),
[Cloudflare](https://www.cloudflare.com/), [Netnod](https://www.netnod.se/),
[SIDN](https://www.sidn.nl/), [Canonical](https://canonical.com/), and
[drand](https://drand.love/) for their service information and policies.

RPC binds to localhost by default. WebSocket, metrics and faucet features are
optional and disabled by default in the node configuration. Exposing a service
on a non-local address changes who can reach it. Faucet/test conveniences do not
make test funds valuable or qualify a public network for production use.

## Local data and removal

Node ledgers and logs remain in the configured data directory. Wallet profiles,
backup files and application settings are separate from the application binary.
Uninstalling an executable does not imply that these files are deleted, and it
does not delete ledger copies held by peers. Back up recovery material before
removing a wallet profile; do not include secrets in public bug reports.

## Implementation references

- [Node configuration and defaults](https://github.com/BURST-UBI/protocol/blob/main/node/src/config.rs)
- [Daemon options](https://github.com/BURST-UBI/protocol/blob/main/daemon/src/main.rs)
- [Auxiliary NTP exchange](https://github.com/BURST-UBI/protocol/blob/main/network/src/clock_sync.rs)
- [Authenticated time sources](https://github.com/BURST-UBI/protocol/blob/main/network/src/trusted_clock/nts.rs)
- [Drand client](https://github.com/BURST-UBI/protocol/blob/main/vrf/src/drand.rs)
- [Local dashboard file operations](https://github.com/BURST-UBI/node-gui/blob/main/src-tauri/src/commands.rs)
