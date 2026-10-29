# Privacy and network behavior

BURST is network software. Its wallet, dashboard and protocol node communicate
with different endpoints. This document describes the current development
candidate; archived release versions may have different behavior.

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
