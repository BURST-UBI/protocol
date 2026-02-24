#!/usr/bin/env bash
set -uo pipefail

# update-testnet.sh — Watch for repo changes and redeploy testnet nodes.
#
# Idempotent and resumable — safe to re-run or interrupt at any time.
# Only nodes that need updating are touched. Failed nodes are retried
# on the next run.
#
# Usage:
#   ./deploy/update-testnet.sh [flags] <seed-ip> [node-ip ...]
#   ./deploy/update-testnet.sh [flags] --nodes-file deploy/testnet-nodes.txt
#
# Flags:
#   --once            Check once and exit (default)
#   --watch [SECS]    Poll continuously (default: every 60s)
#   --force           Rebuild and restart even if already current
#   --nodes-file FILE Read IPs from file
#
# Environment variables:
#   SSH_USER / SSH_PASS / SSH_KEY / BURST_REPO / BURST_BRANCH / BURST_LOG_LEVEL

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

# ── Parse flags ──────────────────────────────────────────────────────
NODES_FILE=""
POSITIONAL=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --once)         MODE="once"; shift ;;
        --watch)
            MODE="watch"
            if [[ "${2:-}" =~ ^[0-9]+$ ]]; then
                POLL_INTERVAL="$2"; shift 2
            else
                shift
            fi
            ;;
        --force)        FORCE=true; shift ;;
        --nodes-file)   NODES_FILE="$2"; shift 2 ;;
        -*)             echo "Unknown flag: $1"; exit 1 ;;
        *)              POSITIONAL+=("$1"); shift ;;
    esac
done

if [ -n "$NODES_FILE" ]; then
    if [ ! -f "$NODES_FILE" ]; then
        echo "Error: nodes file not found: $NODES_FILE"; exit 1
    fi
    while IFS= read -r line; do
        line="${line%%#*}"; line="$(echo "$line" | xargs)"
        [ -n "$line" ] && POSITIONAL+=("$line")
    done < "$NODES_FILE"
fi

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: $0 [--once|--watch [SECS]|--force] [--nodes-file FILE] <seed-ip> [node-ip ...]"
    exit 1
fi

SEED_IP="${POSITIONAL[0]}"
NODE_IPS=("${POSITIONAL[@]:1}")
ALL_IPS=("${POSITIONAL[@]}")

# ── SSH setup ────────────────────────────────────────────────────────
if [ -n "$SSH_PASS" ]; then
    if ! command -v sshpass &>/dev/null; then
        echo "Error: sshpass required for password auth"; exit 1
    fi
    SSH_PREFIX=(sshpass -p "$SSH_PASS")
    SSH_AUTH_OPTS=(-o PubkeyAuthentication=no)
else
    SSH_PREFIX=()
    SSH_AUTH_OPTS=(-i "$SSH_KEY")
fi

ssh_cmd() {
    local host="$1"; shift
    "${SSH_PREFIX[@]}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=15 \
        -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
        "${SSH_AUTH_OPTS[@]}" "${SSH_USER}@${host}" "$@"
}

get_remote_head() {
    git ls-remote "$BURST_REPO" "refs/heads/${BURST_BRANCH}" 2>/dev/null | awk '{print $1}'
}

# ── Per-node update (idempotent) ─────────────────────────────────────

update_node() {
    local host="$1" node_num="$2" is_seed="$3" target_commit="$4"
    local container_name="burst-testnet-${node_num}"

    # Check if node is already at the target commit with container running
    if [ "$FORCE" = "false" ]; then
        local status
        status=$(ssh_cmd "$host" "
            commit='none'
            if [ -d '${REPO_DIR}/.git' ]; then
                commit=\$(cd '${REPO_DIR}' && git rev-parse HEAD 2>/dev/null || echo 'none')
            fi
            img_commit=\$(docker image inspect burst-daemon:testnet --format '{{index .Config.Labels \"burst.commit\"}}' 2>/dev/null || echo 'none')
            running=\$(docker ps -q -f name='^${container_name}\$' 2>/dev/null)
            echo \"\${commit}|\${img_commit}|\${running}\"
        " 2>/dev/null || echo "error||")

        local repo_commit img_commit running
        repo_commit=$(echo "$status" | cut -d'|' -f1)
        img_commit=$(echo "$status" | cut -d'|' -f2)
        running=$(echo "$status" | cut -d'|' -f3)

        if [ "$repo_commit" = "$target_commit" ] && [ "$img_commit" = "$target_commit" ] && [ -n "$running" ]; then
            echo "  [${host}] Already current (${target_commit:0:8}) — skipping"
            return 0
        fi
    fi

    # Step 1: sync repo
    echo "  [${host}] Pulling ${target_commit:0:8}..."
    if ! ssh_cmd "$host" "
        if [ -d '${REPO_DIR}/.git' ]; then
            cd '${REPO_DIR}' && git fetch origin '${BURST_BRANCH}' --quiet && git reset --hard 'origin/${BURST_BRANCH}' --quiet
        else
            git clone --branch '${BURST_BRANCH}' --depth 1 '${BURST_REPO}' '${REPO_DIR}' --quiet
        fi
    " 2>&1; then
        echo "  [${host}] FAILED — repo sync"
        return 1
    fi

    # Step 2: build image
    echo "  [${host}] Building image..."
    if ! ssh_cmd "$host" "cd '${REPO_DIR}' && docker build --label 'burst.commit=${target_commit}' -t burst-daemon:testnet . 2>&1 | tail -3"; then
        echo "  [${host}] FAILED — image build"
        return 1
    fi

    # Step 3: restart container
    echo "  [${host}] Restarting container..."
    ssh_cmd "$host" "docker stop $container_name 2>/dev/null || true; docker rm $container_name 2>/dev/null || true"

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

    if ! ssh_cmd "$host" "${docker_args[*]}"; then
        echo "  [${host}] FAILED — container start"
        return 1
    fi

    # Step 4: verify
    local verify
    verify=$(ssh_cmd "$host" "docker ps -q -f name='^${container_name}\$'" 2>/dev/null || true)
    if [ -n "$verify" ]; then
        echo "  [${host}] OK — running ${target_commit:0:8}"
        return 0
    else
        echo "  [${host}] FAILED — container not running after start"
        return 1
    fi
}

# ── Rolling update across all nodes ──────────────────────────────────

do_update() {
    local target_commit="$1"
    local succeeded=0 failed=0 skipped=0
    local failed_hosts=()

    echo ""
    echo "=== Update to ${target_commit:0:8} — $(date '+%Y-%m-%d %H:%M:%S') ==="
    echo ""

    # Seed first
    echo "--- Seed node ---"
    update_node "$SEED_IP" 1 "true" "$target_commit"
    case $? in
        0) succeeded=$((succeeded + 1)) ;;
        *) failed=$((failed + 1)); failed_hosts+=("$SEED_IP") ;;
    esac

    if [ ${#NODE_IPS[@]} -gt 0 ]; then
        sleep 5
    fi

    local node_num=2
    for ip in "${NODE_IPS[@]}"; do
        echo ""
        echo "--- Node ${node_num} ---"
        update_node "$ip" "$node_num" "false" "$target_commit"
        case $? in
            0) succeeded=$((succeeded + 1)) ;;
            *) failed=$((failed + 1)); failed_hosts+=("$ip") ;;
        esac
        node_num=$((node_num + 1))
    done

    echo ""
    echo "Result: ${succeeded}/${#ALL_IPS[@]} updated"
    if [ $failed -gt 0 ]; then
        echo "Failed: ${failed_hosts[*]} — will retry next run"
    fi

    return $failed
}

# ── Main ─────────────────────────────────────────────────────────────

echo "=== BURST Testnet Updater ==="
echo "Repo:     $BURST_REPO ($BURST_BRANCH)"
echo "Nodes:    ${ALL_IPS[*]}"
echo "Mode:     $MODE"
[ "$MODE" = "watch" ] && echo "Interval: ${POLL_INTERVAL}s"
[ "$FORCE" = "true" ] && echo "Force:    yes"
echo ""

LAST_COMMIT=""

check_and_update() {
    local target_commit
    target_commit=$(get_remote_head)

    if [ -z "$target_commit" ]; then
        echo "[$(date '+%H:%M:%S')] Cannot reach repo — skipping"
        return 1
    fi

    if [ "$FORCE" = "true" ]; then
        echo "[$(date '+%H:%M:%S')] Force update to ${target_commit:0:8}"
        do_update "$target_commit"
        FORCE=false
        LAST_COMMIT="$target_commit"
        return 0
    fi

    if [ "$target_commit" = "$LAST_COMMIT" ]; then
        echo "[$(date '+%H:%M:%S')] No changes (${target_commit:0:8})"
        return 0
    fi

    if [ -z "$LAST_COMMIT" ]; then
        echo "[$(date '+%H:%M:%S')] Current remote: ${target_commit:0:8}"
    else
        echo "[$(date '+%H:%M:%S')] New commit: ${LAST_COMMIT:0:8} -> ${target_commit:0:8}"
    fi

    do_update "$target_commit"
    local result=$?
    LAST_COMMIT="$target_commit"
    return $result
}

case "$MODE" in
    once)
        check_and_update
        ;;
    watch)
        echo "Watching for changes (Ctrl+C to stop)..."
        echo ""
        while true; do
            check_and_update || true
            sleep "$POLL_INTERVAL"
        done
        ;;
esac
