#!/usr/bin/env bash
set -euo pipefail

# health-check.sh — Check the health of BURST testnet nodes.
#
# Usage:
#   ./deploy/health-check.sh <ip1> [ip2 ...]
#
# Queries each node's RPC "node_info" endpoint and reports:
#   - Online/offline status
#   - Block count, account count
#   - Peer count
#   - Uptime
#
# Environment variables:
#   RPC_PORT  — RPC port to query (default: 7077)

RPC_PORT="${RPC_PORT:-7077}"
TIMEOUT=5

if [ $# -lt 1 ]; then
    echo "Usage: $0 <ip1> [ip2 ...]"
    echo ""
    echo "Check health of BURST testnet nodes via RPC."
    exit 1
fi

format_uptime() {
    local secs="$1"
    local days=$((secs / 86400))
    local hours=$(( (secs % 86400) / 3600 ))
    local mins=$(( (secs % 3600) / 60 ))
    if [ "$days" -gt 0 ]; then
        echo "${days}d ${hours}h ${mins}m"
    elif [ "$hours" -gt 0 ]; then
        echo "${hours}h ${mins}m"
    else
        echo "${mins}m"
    fi
}

TOTAL=0
HEALTHY=0

printf "\n%-20s %-8s %10s %10s %6s %12s\n" \
    "NODE" "STATUS" "BLOCKS" "ACCOUNTS" "PEERS" "UPTIME"
printf "%-20s %-8s %10s %10s %6s %12s\n" \
    "----" "------" "------" "--------" "-----" "------"

for host in "$@"; do
    TOTAL=$((TOTAL + 1))

    response=$(curl -s --max-time "$TIMEOUT" \
        -X POST "http://${host}:${RPC_PORT}" \
        -H "Content-Type: application/json" \
        -d '{"action":"node_info"}' 2>/dev/null) || response=""

    if [ -z "$response" ]; then
        printf "%-20s \e[31m%-8s\e[0m %10s %10s %6s %12s\n" \
            "$host" "OFFLINE" "-" "-" "-" "-"
        continue
    fi

    error=$(echo "$response" | jq -r '.error // empty' 2>/dev/null)
    if [ -n "$error" ]; then
        printf "%-20s \e[33m%-8s\e[0m %10s %10s %6s %12s\n" \
            "$host" "ERROR" "-" "-" "-" "$error"
        continue
    fi

    block_count=$(echo "$response" | jq -r '.block_count // 0' 2>/dev/null)
    account_count=$(echo "$response" | jq -r '.account_count // 0' 2>/dev/null)
    peer_count=$(echo "$response" | jq -r '.peer_count // 0' 2>/dev/null)
    uptime_secs=$(echo "$response" | jq -r '.uptime_secs // 0' 2>/dev/null)
    uptime_str=$(format_uptime "$uptime_secs")

    HEALTHY=$((HEALTHY + 1))

    if [ "$peer_count" -eq 0 ]; then
        color="\e[33m"
    else
        color="\e[32m"
    fi

    printf "%-20s ${color}%-8s\e[0m %10s %10s %6s %12s\n" \
        "$host" "OK" "$block_count" "$account_count" "$peer_count" "$uptime_str"
done

echo ""
echo "Summary: ${HEALTHY}/${TOTAL} nodes healthy"

if [ "$HEALTHY" -lt "$TOTAL" ]; then
    exit 1
fi
