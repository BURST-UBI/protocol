#!/usr/bin/env bash
set -euo pipefail

test "$#" -eq 2 || { echo 'Provide candidate mode and creator public key.' >&2; exit 2; }
mode=$1
public_key=${2,,}
fixture_key=197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d61
[[ $public_key =~ ^[0-9a-f]{64}$ ]] || {
  echo 'Creator public key must contain exactly 64 hexadecimal characters.' >&2
  exit 2
}
case "$mode" in
  --private-testnet)
    test "$public_key" != "$fixture_key" || {
      echo 'Published fixture identity requires --private-testnet-fixture; it is not distributable.' >&2
      exit 2
    }
    echo private-testnet
    ;;
  --private-testnet-fixture)
    test "$public_key" = "$fixture_key" || {
      echo 'Fixture mode requires the published disposable fixture identity.' >&2
      exit 2
    }
    echo private-testnet-fixture
    ;;
  *) echo 'Choose --private-testnet or --private-testnet-fixture.' >&2; exit 2;;
esac
