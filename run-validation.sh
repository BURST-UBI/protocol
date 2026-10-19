#!/usr/bin/env bash
set -euo pipefail
export RUST_LOG=burst_node=debug,burst_rpc=debug,info
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
cd "$(dirname "$0")"
# Validate exact prebuilt artifacts. Building here hid which build was tested
# and could start an optimized compiler on the resource-limited desktop.
if [[ $# -lt 2 || $# -gt 3 || "$1" != --artifacts-dir || ( $# == 3 && "$3" != --experimental-testnet && "$3" != --private-testnet && "$3" != --private-testnet-onboarding && "$3" != --private-testnet-bootstrap ) ]]; then
  echo 'Usage: bash run-validation.sh --artifacts-dir /absolute/build/directory [--experimental-testnet|--private-testnet|--private-testnet-onboarding|--private-testnet-bootstrap]' >&2
  exit 2
fi
journey_args=()
native_args=()
if [[ ${BURST_NATIVE_EMBEDDED:-0} == 1 ]]; then native_args+=(--embedded); fi
network=dev
if [[ ${3:-} == --experimental-testnet ]]; then journey_args+=(--experimental-testnet); fi
if [[ ${3:-} == --private-testnet* ]]; then network=test; fi
onboarding=false
bootstrap_only=false
if [[ ${3:-} == --private-testnet-bootstrap ]]; then bootstrap_only=true; fi
if [[ ${3:-} == --private-testnet-onboarding || $bootstrap_only == true ]]; then onboarding=true; fi
artifacts=$(realpath "$2")
binaries=(burst-daemon examples/validate_network examples/validate_restart_payment examples/validate_pending_chain)
if [[ $network == test ]]; then binaries=(burst-daemon examples/validate_testnet_governance); fi
if [[ $onboarding == true ]]; then binaries+=(examples/validate_testnet_onboarding); fi
for binary in "${binaries[@]}"; do
  [[ -x "$artifacts/$binary" ]] || { echo "Missing executable: $artifacts/$binary" >&2; exit 2; }
done
for port in 28076 28077 28086 28087 28096 28097 28106 28107; do
  listeners=$(ss -H -ltn "sport = :$port")
  if [[ -n "$listeners" ]]; then
    echo "Validation port $port is already in use; no nodes started." >&2
    exit 2
  fi
done
evidence_root="${BURST_VALIDATION_ROOT:-$PWD/target/validation-evidence}"
mkdir -p "$evidence_root"
resuming=false
if [[ -n ${BURST_VALIDATION_RESUME:-} ]]; then
  [[ $network == test ]] || { echo 'Resume is only supported for the Test governance journey.' >&2; exit 2; }
  VALIDATION_DIR=$(realpath "$BURST_VALIDATION_RESUME")
  evidence_root=$(realpath "$evidence_root")
  [[ $VALIDATION_DIR == "$evidence_root"/burst-validation.* && -f $VALIDATION_DIR/governance.json && -f $VALIDATION_DIR/artifacts.sha256 ]] || { echo 'Resume requires retained governance evidence in the validation directory.' >&2; exit 2; }
  resuming=true
else
  VALIDATION_DIR=$(mktemp -d "$evidence_root/burst-validation.XXXXXXXX")
fi
echo "Validation logs: $VALIDATION_DIR"
manifest="$VALIDATION_DIR/artifacts.sha256"
if [[ $resuming == true ]]; then manifest=$(mktemp "$VALIDATION_DIR/artifacts-resume.XXXXXXXX.sha256"); fi
for binary in "${binaries[@]}"; do sha256sum "$artifacts/$binary"; done > "$manifest"
umask 077
# Public disposable fixture keys only; each representative has one signer.
for n in 0 1 2; do
  seed=$n
  if [[ $network == test && $n == 0 ]]; then seed=42; fi
  for _ in $(seq 1 32); do printf '%02x' "$seed"; done > "$VALIDATION_DIR/representative$n.key"
done
pids=()
cleanup() { for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done; for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
start_nodes() {
urls=()
pids=()
for n in 0 1 2; do
  rpc=$((28077+n*10)); p2p=$((28076+n*10))
  args=()
  signer=(--representative-key-file "$VALIDATION_DIR/representative$n.key")
  creator_env=(env -u BURST_GENESIS_SEED -u BURST_GENESIS_SEED_FILE)
  if [[ $network == test ]]; then
    if [[ $n == 0 ]]; then
      creator_env+=("BURST_GENESIS_SEED_FILE=$VALIDATION_DIR/representative0.key")
    else
      signer=(--no-representative)
    fi
  fi
  if [ "$n" -gt 0 ]; then args+=(--bootstrap-peers 127.0.0.1:28076); fi
  "${creator_env[@]}" "$artifacts/burst-daemon" --network "$network" --data-dir "$VALIDATION_DIR/node$n" "${signer[@]}" --port "$p2p" --rpc-port "$rpc" --disable-upnp --log-level debug "${args[@]}" node run >> "$VALIDATION_DIR/node$n.log" 2>&1 &
  pids+=("$!"); urls+=("http://127.0.0.1:$rpc")
done
}
ready_nodes() {
# All three nodes start together. Share the former three 30-second readiness
# windows instead of failing the first node while every peer is still replaying.
local deadline=$((SECONDS + 90))
for url in "${urls[@]}"; do
  ready=false
  while (( SECONDS < deadline )); do
    if curl -fsS --max-time 2 "$url" -H 'Content-Type: application/json' -d '{"action":"node_info"}' 2>/dev/null | jq -e '.result' >/dev/null; then ready=true; break; fi
    sleep 0.5
  done
  [ "$ready" = true ] || { echo "Node failed: $url"; exit 1; }
done
}
start_nodes
ready_nodes
sleep 3
signing_ready_nodes() {
  local deadline=$((SECONDS + 90)) url ready params
  for url in "${urls[@]}"; do
    ready=false
    while (( SECONDS < deadline )); do
      params=$(curl -fsS --max-time 2 "$url" -H 'Content-Type: application/json' -d '{"action":"params_hash"}') || params='{}'
      if jq -e '.result.online_signing | . == "local" or . == "peer_matches"' <<< "$params" >/dev/null; then ready=true; break; fi
      sleep 0.5
    done
    if [[ $ready != true ]]; then
      echo "Signing readiness failed: $url $params" >&2
      return 1
    fi
  done
}
if [[ $bootstrap_only == true && $resuming == true ]]; then signing_ready_nodes; fi
if [[ $network == test ]]; then
  if [[ $resuming == false ]]; then
    advance=advance
    if [[ $onboarding == true ]]; then advance=advance-test-accrual; fi
    "$artifacts/examples/validate_testnet_governance" "$advance" "$VALIDATION_DIR/governance.json" "${urls[@]}"
  else
    if [[ ${BURST_NATIVE_EXCLUSION:-0} == 1 && -f $VALIDATION_DIR/native-exclusion.json ]]; then
      node "$BURST_NATIVE_ONBOARDING_WALLET/tests/verify-exclusion.mjs" "$VALIDATION_DIR/native-exclusion.json" "${urls[@]}"
    else
      "$artifacts/examples/validate_testnet_governance" verify "$VALIDATION_DIR/governance.json" "${urls[@]}"
    fi
  fi
  if [[ $onboarding == true ]]; then
    if [[ $bootstrap_only == true ]]; then
      # Governance confirmation can precede peer checkpoint convergence. Wait
      # for the same readiness reported to wallets, without retrying signatures.
      signing_ready_nodes
      "$artifacts/examples/validate_testnet_onboarding" bootstrap-only "$VALIDATION_DIR/bootstrap.json" "${urls[@]}"
      echo 'PASS governed certified helper baseline; consumer, restart and joining checks remain separate'
      exit 0
    fi
    mode=advance
    if [[ ${BURST_ONBOARDING_CONTINUE_SELECTION:-0} == 1 ]]; then mode=continue-selection; fi
    if [[ -f $VALIDATION_DIR/onboarding.json ]]; then mode=verify; fi
    "$artifacts/examples/validate_testnet_onboarding" "$mode" "$VALIDATION_DIR/onboarding.json" "${urls[@]}"
  fi
  if [[ $onboarding == true && -n ${BURST_NATIVE_LIFECYCLE_REPORT:-} ]]; then
    : "${BURST_NATIVE_ONBOARDING_WALLET:?provide the native wallet checkout}"
    if [[ ! -f $VALIDATION_DIR/native-lifecycle.json ]]; then
      NATIVE_FIXTURE_RPC_URL="${urls[0]}" NATIVE_RESULT_PATH="$VALIDATION_DIR/native-lifecycle.json" \
        NATIVE_ONBOARDING_REPORT="$BURST_NATIVE_LIFECYCLE_REPORT" \
        node "$BURST_NATIVE_ONBOARDING_WALLET/tests/native-session.mjs" "${native_args[@]}" --certified-lifecycle
    fi
    "$artifacts/examples/validate_testnet_onboarding" verify "$VALIDATION_DIR/native-lifecycle.json" "${urls[@]}"
  fi
  if [[ ${BURST_NATIVE_GOVERNANCE:-0} == 1 ]]; then
    : "${BURST_NATIVE_ONBOARDING_WALLET:?provide the native wallet checkout}"
    if [[ ! -f $VALIDATION_DIR/native-governance.json ]]; then
      NATIVE_FIXTURE_RPC_URL="${urls[0]}" NATIVE_RESULT_PATH="$VALIDATION_DIR/native-governance.json" \
        node "$BURST_NATIVE_ONBOARDING_WALLET/tests/native-session.mjs" "${native_args[@]}" --certified-governance
    fi
    "$artifacts/examples/validate_testnet_governance" verify "$VALIDATION_DIR/native-governance.json" "${urls[@]}"
  fi
  if [[ ${BURST_NATIVE_EXCLUSION:-0} == 1 ]]; then
    : "${BURST_NATIVE_ONBOARDING_WALLET:?provide the native wallet checkout}"
    if [[ ! -f $VALIDATION_DIR/native-exclusion.json ]]; then
      NATIVE_FIXTURE_RPC_URL="${urls[0]}" NATIVE_RESULT_PATH="$VALIDATION_DIR/native-exclusion.json" \
        node "$BURST_NATIVE_ONBOARDING_WALLET/tests/native-session.mjs" --embedded --certified-exclusion
    fi
    node "$BURST_NATIVE_ONBOARDING_WALLET/tests/verify-exclusion.mjs" "$VALIDATION_DIR/native-exclusion.json" "${urls[@]}"
  fi
  for pid in "${pids[@]}"; do kill -KILL "$pid"; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  start_nodes
  ready_nodes
  if [[ ${BURST_NATIVE_EXCLUSION:-0} != 1 ]]; then "$artifacts/examples/validate_testnet_governance" verify "$VALIDATION_DIR/governance.json" "${urls[@]}"; fi
  if [[ $onboarding == true ]]; then "$artifacts/examples/validate_testnet_onboarding" verify "$VALIDATION_DIR/onboarding.json" "${urls[@]}"; fi
  if [[ -n ${BURST_NATIVE_LIFECYCLE_REPORT:-} ]]; then "$artifacts/examples/validate_testnet_onboarding" verify "$VALIDATION_DIR/native-lifecycle.json" "${urls[@]}"; fi
  echo 'PASS normal Test governance survives hard restart'
  if [[ ${BURST_NATIVE_EXCLUSION:-0} == 1 ]]; then node "$BURST_NATIVE_ONBOARDING_WALLET/tests/verify-exclusion.mjs" "$VALIDATION_DIR/native-exclusion.json" "${urls[@]}"; fi
  if [[ ${BURST_NATIVE_GOVERNANCE:-0} == 1 ]]; then "$artifacts/examples/validate_testnet_governance" verify "$VALIDATION_DIR/native-governance.json" "${urls[@]}"; fi
  observer_dir=$(mktemp -d "$VALIDATION_DIR/observer.XXXXXXXX")
  echo "Fresh Test observer: $observer_dir"
  env -u BURST_GENESIS_SEED -u BURST_GENESIS_SEED_FILE "$artifacts/burst-daemon" --network test --data-dir "$observer_dir/ledger" --no-representative --port 28106 --rpc-port 28107 --disable-upnp --bootstrap-peers 127.0.0.1:28076 node run > "$observer_dir/node.log" 2>&1 &
  pids+=("$!")
  joined=false
  if [[ ${BURST_NATIVE_EXCLUSION:-0} == 1 ]]; then
    node "$BURST_NATIVE_ONBOARDING_WALLET/tests/verify-exclusion.mjs" "$VALIDATION_DIR/native-exclusion.json" http://127.0.0.1:28107
    joined=true
  else
  for attempt in $(seq 1 60); do
    if "$artifacts/examples/validate_testnet_governance" verify "$VALIDATION_DIR/governance.json" http://127.0.0.1:28107 > "$VALIDATION_DIR/late-governance.log" 2>&1; then joined=true; break; fi
    sleep 1
  done
  fi
  if [[ $joined != true ]]; then cat "$VALIDATION_DIR/late-governance.log"; exit 1; fi
  echo 'PASS fresh Test observer recovers certified governance and the activated parameter'
  if [[ ${BURST_NATIVE_GOVERNANCE:-0} == 1 ]]; then "$artifacts/examples/validate_testnet_governance" verify-joining "$VALIDATION_DIR/native-governance.json" http://127.0.0.1:28107; fi
  if [[ $onboarding == true ]]; then "$artifacts/examples/validate_testnet_onboarding" verify-joining "$VALIDATION_DIR/onboarding.json" http://127.0.0.1:28107; fi
  if [[ -n ${BURST_NATIVE_LIFECYCLE_REPORT:-} ]]; then "$artifacts/examples/validate_testnet_onboarding" verify-joining "$VALIDATION_DIR/native-lifecycle.json" http://127.0.0.1:28107; fi
  if [[ $onboarding == true && -n ${BURST_NATIVE_ONBOARDING_WALLET:-} && -z ${BURST_NATIVE_LIFECYCLE_REPORT:-} && ${BURST_NATIVE_EXCLUSION:-0} != 1 ]]; then
    NATIVE_FIXTURE_RPC_URL="${urls[0]}" NATIVE_RESULT_PATH="$VALIDATION_DIR/native-onboarding.json" \
      node "$BURST_NATIVE_ONBOARDING_WALLET/tests/native-session.mjs" "${native_args[@]}" --certified-onboarding
    node "$BURST_NATIVE_ONBOARDING_WALLET/tests/verify-consumer.mjs" "$VALIDATION_DIR/native-onboarding.json" "${urls[@]}"
  fi
  exit 0
fi
BURST_VALIDATION_REPORT="$VALIDATION_DIR/expected.json" "$artifacts/examples/validate_network" "${journey_args[@]}" "${urls[@]}"
# A hard stop catches economic state saved only by graceful shutdown.
for pid in "${pids[@]}"; do kill -KILL "$pid"; done
for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
start_nodes
ready_nodes
"$artifacts/examples/validate_network" --verify "$VALIDATION_DIR/expected.json" "${urls[@]}"
echo "PASS crash/restart validation; evidence retained in $VALIDATION_DIR"
"$artifacts/examples/validate_restart_payment" "${urls[@]}"
# A fresh late joiner must reconstruct the same economic state from peers.
"$artifacts/burst-daemon" --network dev --data-dir "$VALIDATION_DIR/node3" --no-representative --port 28106 --rpc-port 28107 --disable-upnp --bootstrap-peers 127.0.0.1:28076 node run > "$VALIDATION_DIR/node3.log" 2>&1 &
pids+=("$!")
joined=false
for attempt in $(seq 1 60); do
  if "$artifacts/examples/validate_network" --verify "$VALIDATION_DIR/expected.json" --epoch-source "${urls[0]}" http://127.0.0.1:28107 > "$VALIDATION_DIR/late-join.log" 2>&1; then joined=true; break; fi
  sleep 1
done
if [ "$joined" != true ]; then cat "$VALIDATION_DIR/late-join.log"; exit 1; fi
echo "PASS fresh late-joining node reconstructs the same ledger and economic state"
# Only Bob's representative remains online: one of the three verified humans.
"$artifacts/examples/validate_pending_chain" ready "$VALIDATION_DIR/pending-chain.json" "${urls[@]}"
for n in 0 1 3; do kill "${pids[$n]}"; done
for n in 0 1 3; do wait "${pids[$n]}" 2>/dev/null || true; done
pids=("${pids[2]}")
"$artifacts/examples/validate_pending_chain" stage "$VALIDATION_DIR/pending-chain.json" "${urls[2]}"
kill -KILL "${pids[0]}"
wait "${pids[0]}" 2>/dev/null || true
"$artifacts/burst-daemon" --network dev --data-dir "$VALIDATION_DIR/node2" \
  --representative-key-file "$VALIDATION_DIR/representative2.key" \
  --port 28096 --rpc-port 28097 --disable-upnp node run >> "$VALIDATION_DIR/node2.log" 2>&1 &
pids=("$!")
urls=(http://127.0.0.1:28097)
ready_nodes
"$artifacts/examples/validate_pending_chain" verify-isolated "$VALIDATION_DIR/pending-chain.json" "${urls[0]}"
kill "${pids[0]}"
wait "${pids[0]}" 2>/dev/null || true
start_nodes
ready_nodes
"$artifacts/examples/validate_pending_chain" verify "$VALIDATION_DIR/pending-chain.json" "${urls[@]}"
