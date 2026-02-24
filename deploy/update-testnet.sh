#!/usr/bin/env bash
set -uo pipefail

# update-testnet.sh — Manual force-update for BURST testnet nodes.
#
# NOTE: Watchtower runs on each VPS and auto-updates containers when
# CI pushes a new image. This script is only needed for manual
# force-updates or if Watchtower is not running.
#
# Pulls the latest Docker image on each node and restarts containers.
#
# Usage:
#   ./deploy/update-testnet.sh [flags] <seed-ip> [node-ip ...]
#   ./deploy/update-testnet.sh [flags] --nodes-file deploy/testnet-nodes.txt
#
# Flags:
#   --once            Check once and exit (default)
#   --watch [SECS]    Poll continuously (default: every 60s)
#   --force           Pull and restart even if already current
#   --nodes-file FILE Read IPs from file
#   --tag TAG         Docker image tag (default: latest)
#
# Environment variables:
#   SSH_USER / SSH_PASS / SSH_KEY / BURST_LOG_LEVEL

SSH_USER="${SSH_USER:-root}"
SSH_PASS="${SSH_PASS:-}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
BURST_LOG_LEVEL="${BURST_LOG_LEVEL:-info}"

MODE="once"
POLL_INTERVAL=60
FORCE=false
IMAGE_TAG="latest"

DOCKER_IMAGE="ghcr.io/burst-ubi/protocol"

STATE_DIR="${HOME}/.burst-deploy"

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
        --tag)          IMAGE_TAG="$2"; shift 2 ;;
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
    echo "Usage: $0 [--once|--watch [SECS]|--force] [--nodes-file FILE] [--tag TAG] <seed-ip> [node-ip ...]"
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

mkdir -p "$STATE_DIR"

# ── Per-node update (idempotent) ─────────────────────────────────────

update_node() {
    local host="$1" node_num="$2" is_seed="$3"
    local container_name="burst-testnet-${node_num}"
    local full_image="${DOCKER_IMAGE}:${IMAGE_TAG}"

    # Get current image ID before pull
    local old_id
    old_id=$(ssh_cmd "$host" "docker image inspect '${full_image}' --format '{{.Id}}' 2>/dev/null || echo 'none'" 2>/dev/null)

    if [ "$FORCE" = "false" ]; then
        # Pull to check for updates
        if ! ssh_cmd "$host" "docker pull '${full_image}'" &>/dev/null; then
            echo "  [${host}] FAILED — docker pull"
            return 1
        fi

        local new_id
        new_id=$(ssh_cmd "$host" "docker image inspect '${full_image}' --format '{{.Id}}' 2>/dev/null || echo 'none'" 2>/dev/null)

        # Check if running and already on latest image
        local running
        running=$(ssh_cmd "$host" "docker ps -q -f name='^${container_name}\$'" 2>/dev/null || true)

        if [ "$old_id" = "$new_id" ] && [ -n "$running" ]; then
            echo "  [${host}] Already current — skipping"
            return 0
        fi
    else
        if ! ssh_cmd "$host" "docker pull '${full_image}'" &>/dev/null; then
            echo "  [${host}] FAILED — docker pull"
            return 1
        fi
    fi

    # Restart container with new image
    echo "  [${host}] Restarting container..."
    ssh_cmd "$host" "docker stop $container_name 2>/dev/null || true; docker rm $container_name 2>/dev/null || true" &>/dev/null

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

    docker_args+=("${full_image}")

    if ! ssh_cmd "$host" "${docker_args[*]}" &>/dev/null; then
        echo "  [${host}] FAILED — container start"
        return 1
    fi

    sleep 2
    local verify
    verify=$(ssh_cmd "$host" "docker ps -q -f name='^${container_name}\$'" 2>/dev/null || true)
    if [ -n "$verify" ]; then
        echo "  [${host}] OK — updated and running"
        return 0
    else
        echo "  [${host}] FAILED — container not running after start"
        return 1
    fi
}

# ── Rolling update across all nodes ──────────────────────────────────

do_update() {
    local succeeded=0 failed=0
    local failed_hosts=()

    echo ""
    echo "=== Update — $(date '+%Y-%m-%d %H:%M:%S') ==="
    echo ""

    echo "--- Seed node ---"
    update_node "$SEED_IP" 1 "true"
    case $? in
        0) succeeded=$((succeeded + 1)) ;;
        *) failed=$((failed + 1)); failed_hosts+=("$SEED_IP") ;;
    esac

    if [ ${#NODE_IPS[@]} -gt 0 ] && [ $failed -eq 0 ]; then
        sleep 5

        echo ""
        echo "--- Updating nodes 2-$((${#NODE_IPS[@]} + 1)) in parallel ---"

        local pids=()
        local node_num=2
        for ip in "${NODE_IPS[@]}"; do
            (
                update_node "$ip" "$node_num" "false"
                echo $? > "${STATE_DIR}/${ip}.update_result"
            ) &
            pids+=($!)
            node_num=$((node_num + 1))
        done

        for pid in "${pids[@]}"; do
            wait "$pid" 2>/dev/null || true
        done

        node_num=2
        for ip in "${NODE_IPS[@]}"; do
            local rc=1
            [ -f "${STATE_DIR}/${ip}.update_result" ] && rc=$(cat "${STATE_DIR}/${ip}.update_result")
            rm -f "${STATE_DIR}/${ip}.update_result"
            case $rc in
                0) succeeded=$((succeeded + 1)) ;;
                *) failed=$((failed + 1)); failed_hosts+=("$ip") ;;
            esac
            node_num=$((node_num + 1))
        done
    fi

    echo ""
    echo "Result: ${succeeded}/${#ALL_IPS[@]} updated"
    if [ $failed -gt 0 ]; then
        echo "Failed: ${failed_hosts[*]} — will retry next run"
    fi

    return $failed
}

# ── Main ─────────────────────────────────────────────────────────────

echo "=== BURST Testnet Updater ==="
echo "Image:    ${DOCKER_IMAGE}:${IMAGE_TAG}"
echo "Nodes:    ${ALL_IPS[*]}"
echo "Mode:     $MODE"
[ "$MODE" = "watch" ] && echo "Interval: ${POLL_INTERVAL}s"
[ "$FORCE" = "true" ] && echo "Force:    yes"
echo ""

case "$MODE" in
    once)
        do_update
        ;;
    watch)
        echo "Watching for image updates (Ctrl+C to stop)..."
        echo ""
        while true; do
            do_update || true
            FORCE=false
            sleep "$POLL_INTERVAL"
        done
        ;;
esac
