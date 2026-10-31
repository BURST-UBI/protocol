# Preparing a network launch

A network launch needs agreed public inputs, securely held signing keys and
qualified software. Building a daemon or starting a server does not establish
that a Live network is ready. Test candidates qualify only their recorded scope.

## Freeze the public identity

Every participant must agree on the creator public key, configured genesis hash,
initial parameters and supported wire/block versions. Publish those public inputs
with the release. Bootstrap addresses help nodes connect; they do not authorize a
different genesis.

The running node derives its anchor through `configured_genesis_block`. Historical
genesis fixture helpers are not launch manifests. Changing the public key or
initial parameters changes the configured network identity.

## Resolve the starting balance

The current configured genesis uses Unix epoch zero. The creator's initial
verified time is also zero, with no burned or staked BRN. At the default accrual
rate, this supplies decades of accrued allowance. It is an economic property of
the current initialization, not merely a placeholder date on a block.

Creator endorsements still burn the normal endorsement amount: 336 BRN by
default. Adding 49 people to reach a total of 50 costs 16,464 BRN. A fresh account
accruing about 1 BRN/hour would need about 686 days to accumulate that amount,
ignoring other spending.

Before launching, explicitly accept or change the starting accrual rule and
qualify the resulting network inputs. Setting the expiry period to unlimited does
not create supply, and the creator verification shortcut does not waive the burn.
Do not fund bootstrap by modifying a live database outside the protocol.

## Keep signing authority private

Public keys and genesis hashes belong in a public manifest. Seeds do not.
Only the authority node needs the matching creator seed file. Ordinary peers
must not receive it.

Generate the creator identity on a trusted device:

```sh
burst-daemon genesis-keygen live --seed-file /private/genesis.seed
```

The command creates a new private seed file on Unix and prints public identity
information. It does not load node configuration, open a ledger or start networking.
Preserve a private backup and verify the resulting public identity. Do
not print secrets into CI logs or replace an existing network identity casually.
Protect the administrative RPC separately from the creator key.

Run one durable signing database per voting identity. Independent copies of that
identity are not redundant independent representatives and can violate signing
history assumptions.

## Qualify and connect the nodes

Use explicit Live configuration, a fresh Live data directory and the agreed
parameters. Private Test/Dev ledgers, test seeds and reduced thresholds are not
launch inputs. Check the exact candidate's feature restrictions and qualification
record before enabling public service.

Give peers reachable P2P addresses. Keep administrative RPC private; opening a
P2P port does not require exposing wallet or administrative APIs. Operators must
also provide the clock and randomness connectivity their configuration requires.
Verify that independently operated nodes agree on genesis, parameters and
confirmed state, and that their backups and recovery procedures work.

## Enroll the first participants

Each participant creates and controls their own wallet keys. During bootstrap,
the creator can submit `genesis_endorse` for a participant's public address through
authorized local RPC. This builds a signed endorsement and spends BRN. Verify
confirmation and the participant's resulting state before retrying an uncertain
submission; an HTTP success alone is insufficient.

The default exit is 50 verified accounts including the creator. Exit is permanent.
Ordinary verification needs three endorsements and seven eligible verifiers by
default. Verifiers also need the required age, available balance and vote stake.
Recruiting enough accounts therefore does not by itself make ordinary enrollment
usable. See [parameters](../protocol/PARAMETERS.md) and [bootstrap authority](../protocol/BOOTSTRAP.md).

## Source

- [Configured genesis encoding](https://github.com/BURST-UBI/protocol/blob/main/ledger/src/genesis.rs)
- [Genesis account initialization](https://github.com/BURST-UBI/protocol/blob/main/node/src/genesis_state.rs)
- [Creator endorsement RPC](https://github.com/BURST-UBI/protocol/blob/main/rpc/src/handlers.rs)
- [Key generation tool](https://github.com/BURST-UBI/protocol/blob/main/daemon/src/genesis_keygen.rs)
- [Release candidate scope](../release/DOWNLOADS.md)
