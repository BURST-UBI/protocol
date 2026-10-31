#!/usr/bin/env bash
# Poll node_info until every endpoint reports the requested network.
# This establishes RPC availability, not synchronization, signing authority or
# readiness for every wallet operation. Scenario assertions must check those.
wait_for_node_rpcs() {
  local network=$1 budget=$2
  shift 2
  if timeout "$budget" bash -c '
    set -uo pipefail
    network=$1; shift
    while true; do
      ready=true
      for endpoint in "$@"; do
        if ! curl -fsS --max-time 2 "$endpoint" \
          -H "Content-Type: application/json" -d '\''{"action":"node_info"}'\'' 2>/dev/null |
          jq -e --arg network "$network" '\''.result.network == $network'\'' >/dev/null 2>&1; then
          ready=false
        fi
      done
      if "$ready"; then exit 0; fi
      sleep 0.2
    done
  ' bash "$network" "$@"; then
    return 0
  fi
  echo "RPC readiness failed for $network within ${budget}s: $*" >&2
  return 1
}
