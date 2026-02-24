#!/usr/bin/env bash
set -euo pipefail

# update-testnet.sh — Watch for repo changes and redeploy testnet nodes.
#
# Usage:
#   ./deploy/update-testnet.sh <seed-ip> [node-ip ...]
#
# Checks the remote GitHub repo for new commits on the configured branch.
# If changes are found, pulls on each VPS, rebuilds, and restarts the nodes.
#
# Modes:
#   --once          Check once and exit (default)
#   --watch [SECS]  Poll continuously (default: every 60 seconds)
#   --force         Rebuild and restart even if no changes detected
#
# Environment variables:
#   SSH_USER          — remote user (default: root)
#   SSH_PASS          — SSH password (if set, uses password auth via sshpass)
#   SSH_KEY           — path to SSH private key (default: ~/.ssh/id_ed25519)
#   BURST_REPO        — git repo URL (default: https://github.com/BURST-UBI/protocol.git)
#   BURST_BRANCH      — branch to watch (default: main)
#   BURST_LOG_LEVEL   — log level for all nodes (default: info)

SSH_USER="${SSH_USER:-root}"
SSH_PASS="${SSH_PASS:-}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
BURST_REPO="${BURST_REPO:-https://github.com/BURST-UBI/protocol.git}"
BURST_BRANCH="${BURST_BRANCH:-main}"
BURST_LOG_LEVEL="${BURST_LOG_LEVEL:-info}"

MODE="once"
POLL_INTERVAL=60
FORCE=false

REPO_DIR="/opt/burst-protocol"

P2P_PORT=17076
RPC_PORT=7077
WS_PORT=7078

# Parse flags before IPs
while [[ $# -gt 0 ]]; do
    case "$1" in
        --once)
            MODE="once"
            shift
            ;;
        --watch)
            MODE="watch"
            if [[ "${2:-}" =~ ^[0-9]+$ ]]; then
                POLL_INTERVAL="$2"
                shift 2
            else
                shift
            fi
            ;;
        --force)
            FORCE=true
            shift
            ;;
        -*)
            echo "Unknown flag: $1"
            exit 1
            ;;
        *)
            break
            ;;
    esac
done

if [ $# -lt 1 ]; then
    echo "Usage: $0 [--once|--watch [SECS]|--force] <seed-ip> [node-ip ...]"
    exit 1
fi

SEED_IP="$1"
shift
NODE_IPS=("$@")
ALL_IPS=("$SEED_IP" "${NODE_IPS[@]}")

if [ -n "$SSH_PASS" ]; then
    if ! command -v sshpass &>/dev/null; then
        echo "Error: sshpass required for password auth"
        exit 1
    fi
    SSH_PREFIX=(sshpass -p "$SSH_PASS")
    SSH_AUTH_OPTS=(-o PubkeyAuthentication=no)
else
    SSH_PREFIX=()
    SSH_AUTH_OPTS=(-i "$SSH_KEY")
fi

ssh_cmd() {
    local host="$1"
    shift
    "${SSH_PREFIX[@]}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
        "${SSH_AUTH_OPTS[@]}" "${SSH_USER}@${host}" "$@"
}

LAST_KNOWN_COMMIT=""

get_remote_head() {
    git ls-remote "$BURST_REPO" "refs/heads/${BURST_BRANCH}" 2>/dev/null | awk '{print $1}'
}

get_node_commit() {
    local host="$1"
    ssh_cmd "$host" "cd '${REPO_DIR}' && git rev-parse HEAD 2>/dev/null || echo 'none'"
}

update_node() {
    local host="$1"
    local node_num="$2"
    local is_seed="$3"
    local container_name="burst-testnet-${node_num}"

    echo "  [${host}] Pulling latest..."
    ssh_cmd "$host" "cd '${REPO_DIR}' && git fetch origin '${BURST_BRANCH}' --quiet && git reset --hard 'origin/${BURST_BRANCH}' --quiet"

    echo "  [${host}] Rebuilding image..."
    ssh_cmd "$host" "cd '${REPO_DIR}' && docker build -t burst-daemon:testnet . 2>&1 | tail -3"

    echo "  [${host}] Restarting container..."
    ssh_cmd "$host" "docker stop $container_name 2>/dev/null || true"
    ssh_cmd "$host" "docker rm $container_name 2>/dev/null || true"

    local docker_args=(
        "docker" "run" "-d"
        "--name" "$container_name"
        "--restart" "unless-stopped"
        "-v" "burst-testnet-data:/home/burst/data"
        "-p" "${P2P_PORT}:${P2P_PORT}"
        "-e" "BURST_NETWORK=test"
        "-e" "BURST_LOG_LEVEL=${BURST_LOG_LEVEL}"
        "-e" "BURST_ENABLE_WEBSOCKET=true"
        "-e" "BURST_ENABLE_METRICS=true"
    )

    if [ "$is_seed" = "true" ]; then
        docker_args+=(
            "-p" "${RPC_PORT}:${RPC_PORT}"
            "-p" "${WS_PORT}:${WS_PORT}"
            "-e" "BURST_ENABLE_FAUCET=true"
        )
    else
        docker_args+=(
            "-e" "BURST_BOOTSTRAP_PEERS=${SEED_IP}:${P2P_PORT}"
        )
    fi

    docker_args+=("burst-daemon:testnet")
    ssh_cmd "$host" "${docker_args[*]}"
    echo "  [${host}] Node ${node_num} updated and running"
}

rolling_update() {
    local remote_commit="$1"
    echo ""
    echo "=== Rolling update to ${remote_commit:0:8} ==="
    echo "    $(date '+%Y-%m-%d %H:%M:%S')"
    echo ""

    # Update seed first
    echo "--- Updating seed node ---"
    update_node "$SEED_IP" 1 "true"

    echo ""
    echo "Waiting 5s for seed to stabilize..."
    sleep 5

    # Update remaining nodes
    local node_num=2
    for ip in "${NODE_IPS[@]}"; do
        echo ""
        echo "--- Updating node ${node_num} ---"
        update_node "$ip" "$node_num" "false"
        node_num=$((node_num + 1))
    done

    LAST_KNOWN_COMMIT="$remote_commit"

    echo ""
    echo "=== Update complete ==="
    echo "All nodes running commit ${remote_commit:0:8}"
    echo ""
}

check_and_update() {
    local remote_commit
    remote_commit=$(get_remote_head)

    if [ -z "$remote_commit" ]; then
        echo "[$(date '+%H:%M:%S')] Failed to query remote — skipping"
        return 1
    fi

    if [ "$FORCE" = "true" ]; then
        echo "[$(date '+%H:%M:%S')] Force update requested"
        rolling_update "$remote_commit"
        FORCE=false
        return 0
    fi

    if [ -z "$LAST_KNOWN_COMMIT" ]; then
        LAST_KNOWN_COMMIT=$(get_node_commit "$SEED_IP")
        echo "[$(date '+%H:%M:%S')] Seed is at ${LAST_KNOWN_COMMIT:0:8}, remote is ${remote_commit:0:8}"
    fi

    if [ "$remote_commit" != "$LAST_KNOWN_COMMIT" ]; then
        echo "[$(date '+%H:%M:%S')] New commit detected: ${LAST_KNOWN_COMMIT:0:8} -> ${remote_commit:0:8}"
        rolling_update "$remote_commit"
        return 0
    else
        echo "[$(date '+%H:%M:%S')] No changes (${remote_commit:0:8})"
        return 1
    fi
}

# --- Main ---

echo "=== BURST Testnet Updater ==="
echo "Repo:     $BURST_REPO"
echo "Branch:   $BURST_BRANCH"
echo "Nodes:    ${ALL_IPS[*]}"
echo "Mode:     $MODE"
if [ "$MODE" = "watch" ]; then
    echo "Interval: ${POLL_INTERVAL}s"
fi
echo ""

if [ "$MODE" = "once" ]; then
    check_and_update
elif [ "$MODE" = "watch" ]; then
    echo "Watching for changes (Ctrl+C to stop)..."
    echo ""
    while true; do
        check_and_update || true
        sleep "$POLL_INTERVAL"
    done
fi
