#!/usr/bin/env bash
set -euo pipefail

if (( $# == 0 )); then
  echo 'Usage: bash tools/limited-run.sh <command> [arguments...]' >&2
  exit 2
fi

if [[ "$(uname -s)" != Linux ]] || ! command -v systemd-run >/dev/null; then
  echo 'Local validation requires a systemd user manager with memory/CPU limits; no uncapped fallback.' >&2
  exit 2
fi

available_kib=$(awk '/^MemAvailable:/ { print $2 }' /proc/meminfo)
if [[ ! "$available_kib" =~ ^[0-9]+$ ]] || (( available_kib < 4 * 1024 * 1024 )); then
  echo 'Less than 4 GiB is available; deferring local validation instead of adding memory pressure.' >&2
  exit 75
fi

# A fixed unit name rejects overlapping jobs across the wallet, node and protocol.
# Limits cover the entire process tree, including compiler/linker subprocesses.
exec systemd-run --user --quiet --wait --pipe --collect --expand-environment=no \
  --unit=burst-local-validation --service-type=exec \
  --working-directory="$PWD" \
  -p MemoryHigh=2500M -p MemoryMax=3G -p MemorySwapMax=0 \
  -p CPUQuota=100% -p TasksMax=64 -p Nice=19 -p IOWeight=10 \
  -p OOMPolicy=kill -p RuntimeMaxSec=20min -p TimeoutStopSec=10s \
  --setenv="PATH=$PATH" \
  --setenv=CARGO_BUILD_JOBS=1 --setenv=BURST_TEST_CPUS=1 \
  --setenv=BURST_TEST_NETWORK_CONCURRENCY=1 --setenv=TOKIO_WORKER_THREADS=1 \
  --setenv=RAYON_NUM_THREADS=1 --setenv=RUST_TEST_THREADS=1 \
  -- "$@"
