#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

mode="${1:-fast}"
listing=()
usage() {
  echo 'Usage: bash tools/test-node.sh [fast|slow|network|all] [--list]' >&2
  echo '       bash tools/test-node.sh focused <exact-test-name> [--list]' >&2
  exit 2
}
focused=""
if [[ "$mode" == focused ]]; then
  focused="${2:-}"
  [[ "$focused" =~ ^[[:alnum:]_:]+$ && $# -le 3 ]] || usage
  list_option="${3:-}"
else
  (( $# <= 2 )) || usage
  list_option="${2:-}"
fi
case "$list_option" in
  "") ;;
  --list) listing=(--list) ;;
  *) usage ;;
esac
case "$mode" in
  fast|slow|network|all|focused) ;;
  *) usage ;;
esac

cpu_budget="${BURST_TEST_CPUS:-2}"
if [[ ! "$cpu_budget" =~ ^[1-9][0-9]{0,3}$ ]]; then
  echo 'BURST_TEST_CPUS must be a positive integer below 10000' >&2
  exit 2
fi
runner=(nice -n 15)
if [[ -r /proc/self/status ]]; then
  command -v taskset >/dev/null || { echo 'taskset is required to enforce the Linux test CPU budget' >&2; exit 2; }
  allowed=""
  while IFS=: read -r key value; do
    if [[ "$key" == Cpus_allowed_list ]]; then
      allowed="${value//[[:space:]]/}"
      break
    fi
  done < /proc/self/status
  [[ -n "$allowed" ]] || { echo 'Cannot determine allowed test CPUs' >&2; exit 2; }
  IFS=, read -r -a ranges <<< "$allowed"
  cpus=()
  for range in "${ranges[@]}"; do
    first="${range%-*}"
    last="${range#*-}"
    for (( cpu=first; cpu<=last && ${#cpus[@]}<cpu_budget; cpu++ )); do
      cpus+=("$cpu")
    done
  done
  cpu_budget="${#cpus[@]}"
  cpu_list=$(IFS=,; echo "${cpus[*]}")
  runner+=(taskset -c "$cpu_list")
  echo "Test CPU affinity: $cpu_list; low priority (nice +15)" >&2
else
  echo 'CPU affinity is unavailable here; build and test worker limits still apply.' >&2
fi
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export TOKIO_WORKER_THREADS="${TOKIO_WORKER_THREADS:-$cpu_budget}"
node_tests=(cargo test -p burst-node --lib --locked --profile validation --)
if [[ -n "${BURST_NODE_TEST_BINARY:-}" ]]; then
  if [[ "$BURST_NODE_TEST_BINARY" != /* || ! -f "$BURST_NODE_TEST_BINARY" || ! -x "$BURST_NODE_TEST_BINARY" ]]; then
    echo 'BURST_NODE_TEST_BINARY must name an absolute executable file' >&2
    exit 2
  fi
  node_tests=("$BURST_NODE_TEST_BINARY")
fi

shard_index="${BURST_TEST_NETWORK_SHARD:-0}"
shard_count="${BURST_TEST_NETWORK_SHARDS:-1}"
if [[ ! "$shard_count" =~ ^[1-9][0-9]?$ || ! "$shard_index" =~ ^(0|[1-9][0-9]?)$ ]] \
    || (( shard_index >= shard_count )); then
  echo 'Network shard must be a zero-based index below BURST_TEST_NETWORK_SHARDS (1–99)' >&2
  exit 2
fi
if (( shard_count > 1 )) && [[ "$mode" != network ]]; then
  echo 'Network sharding is only supported in network mode' >&2
  exit 2
fi

# Keep permit-waiting network fixtures and expensive replay out of fast workers.
network_filters=(
  '::compact_live_challenge_archive_'
  '::compact_live_appeal_archive_'
  'confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::'
  'confirmed_replay::tests::certified_votes::activation::governance::compact_governance_service_agrees_and_recovers_over_tcp'
  'confirmed_replay::tests::certified_votes::activation::compact_activation_service_agrees_and_recovers_over_tcp'
  'confirmed_replay::tests::certified_votes::challenges::outcomes::chain::compact_outcome_service_agrees_and_recovers_over_tcp'
  'confirmed_replay::tests::snapshot_history::selection::compact_service::'
  'certified_network_tests::'
  'confirmed_replay::tests::certified_votes::installation::process_crash::publication::'
  'confirmed_replay::tests::checkpoint_service_agrees_over_tcp_only_after_seven_of_nine_humans_join'
  'confirmed_replay::tests::checkpoint_service_recovers_expired_minority_selection_over_tcp'
  'confirmed_replay::tests::snapshot_export::snapshot_peer_transfer_stages_authenticated_archive_over_tcp_and_restarts'
  'confirmed_replay::tests::checkpoint_runtime_builds_candidates_and_advances_late_nodes_over_tcp'
  '::outcomes::transport::'
  '::transfer::wire::'
)
slow_filters=(
  'confirmed_replay::tests::electorate::'
  '::signed_electorate_grows_across_snapshot_epochs_and_reopens'
  '::electorate_resume_preserves_source_and_rejects_uncheckpointed_tail'
  '::accepted_electorate_recovery_projects_state_before_strict_resume'
  '::stored_suffix_recovery_authenticates_before_acceptance_and_reaches_final_target'
  'confirmed_replay::tests::snapshot_history::installed_recovery::publication_crash::'
  'confirmed_replay::tests::certified_votes::challenges::deadline::challenge_deadline_and_pending_expiry_preserve_fraud_and_inactivity_accounting'
)
skip_expensive=()
for filter in "${network_filters[@]}" "${slow_filters[@]}"; do
  skip_expensive+=(--skip "$filter")
done

network_threads="${BURST_TEST_NETWORK_CONCURRENCY:-1}"
if [[ "$mode" == network || "$mode" == all ]]; then
  case "$network_threads" in
    1) ;;
    2) ulimit -n 4096 ;;
    *) echo 'BURST_TEST_NETWORK_CONCURRENCY must be 1 or 2' >&2; exit 2 ;;
  esac
fi

if [[ "$mode" == focused ]]; then
  inventory=$("${runner[@]}" "${node_tests[@]}" "$focused" --exact --list)
  if [[ $'\n'"$inventory"$'\n' != *$'\n'"$focused: test"$'\n'* ]]; then
    echo "No exact node library test named: $focused" >&2
    exit 2
  fi
  if (( ${#listing[@]} )); then
    printf '%s\n' "$inventory"
  else
    "${runner[@]}" "${node_tests[@]}" "$focused" --exact --test-threads=1
  fi
fi
if [[ "$mode" == fast || "$mode" == all ]]; then
  "${runner[@]}" "${node_tests[@]}" "${skip_expensive[@]}" --test-threads="$cpu_budget" "${listing[@]}"
fi
if [[ "$mode" == slow || "$mode" == all ]]; then
  "${runner[@]}" "${node_tests[@]}" "${slow_filters[@]}" --test-threads=1 "${listing[@]}"
fi
if [[ "$mode" == network || "$mode" == all ]]; then
  if (( shard_count == 1 )); then
    "${runner[@]}" "${node_tests[@]}" "${network_filters[@]}" \
      --test-threads="$network_threads" "${listing[@]}"
  else
    inventory=$("${runner[@]}" "${node_tests[@]}" "${network_filters[@]}" --list)
    selected=()
    position=0
    while IFS= read -r name; do
      [[ "$name" =~ ^[[:alnum:]_:]+:\ test$ ]] || continue
      name="${name%: test}"
      if (( position % shard_count == shard_index )); then selected+=("$name"); fi
      position=$((position + 1))
    done < <(LC_ALL=C sort <<< "$inventory")
    (( position > 0 )) || { echo 'No network tests were discovered' >&2; exit 1; }
    echo "Network shard $shard_index/$shard_count: ${#selected[@]} of $position tests" >&2
    failures=()
    for name in "${selected[@]}"; do
      if (( ${#listing[@]} )); then
        printf '%s: test\n' "$name"
      else
        if ! "${runner[@]}" "${node_tests[@]}" "$name" --exact --test-threads=1; then
          failures+=("$name")
        fi
      fi
    done
    if (( ${#failures[@]} )); then
      printf 'Failed network test: %s\n' "${failures[@]}" >&2
      exit 1
    fi
  fi
fi
