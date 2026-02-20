#!/usr/bin/env bash
set -euo pipefail

# deploy-testnet.sh — Deploy a BURST testnet across multiple VPSes.
#
# Usage:
#   ./deploy/deploy-testnet.sh <seed-ip> [node-ip ...]
#
# The first IP becomes the seed node (with RPC/WS/faucet enabled).
# All subsequent nodes bootstrap from the seed.
#
# Prerequisites:
#   - SSH access to all VPSes (key-based, as current user or via SSH_USER)
#   - Docker installed on all VPSes (or use deploy/setup-node.sh first)
#
# Environment variables:
#   SSH_USER          — remote user (default: root)
#   SSH_KEY           — path to SSH private key (default: ~/.ssh/id_ed25519)
#   DOCKER_IMAGE      — pre-built image name (default: builds on each VPS)
#   BURST_LOG_LEVEL   — log level for all nodes (default: info)

SSH_USER="${SSH_USER:-root}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
BURST_LOG_LEVEL="${BURST_LOG_LEVEL:-info}"
DOCKER_IMAGE="${DOCKER_IMAGE:-}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

P2P_PORT=17076
RPC_PORT=7077
WS_PORT=7078

ssh_cmd() {
    local host="$1"
    shift
    ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
        -i "$SSH_KEY" "${SSH_USER}@${host}" "$@"
}

scp_cmd() {
    scp -o StrictHostKeyChecking=no -o ConnectTimeout=10 \
        -i "$SSH_KEY" "$@"
}

usage() {
    echo "Usage: $0 <seed-ip> [node-ip ...]"
    echo ""
    echo "Deploy a BURST testnet. First IP is the seed node."
    echo ""
    echo "Examples:"
    echo "  $0 10.0.0.1 10.0.0.2 10.0.0.3 10.0.0.4 10.0.0.5"
    echo "  SSH_USER=ubuntu $0 seed.example.com node2.example.com"
    exit 1
}

if [ $# -lt 1 ]; then
    usage
fi

SEED_IP="$1"
shift
NODE_IPS=("$@")
ALL_IPS=("$SEED_IP" "${NODE_IPS[@]}")

echo "=== BURST Testnet Deployment ==="
echo "Seed node:  $SEED_IP"
echo "Other nodes: ${NODE_IPS[*]:-none}"
echo "SSH user:   $SSH_USER"
echo ""

deploy_node() {
    local host="$1"
    local node_num="$2"
    local is_seed="$3"
    local container_name="burst-testnet-${node_num}"

    echo "--- Deploying node ${node_num} to ${host} ---"

    ssh_cmd "$host" "docker stop $container_name 2>/dev/null || true"
    ssh_cmd "$host" "docker rm $container_name 2>/dev/null || true"

    if [ -n "$DOCKER_IMAGE" ]; then
        echo "  Using pre-built image: $DOCKER_IMAGE"
        ssh_cmd "$host" "docker pull $DOCKER_IMAGE"
        local image="$DOCKER_IMAGE"
    else
        echo "  Building image on remote host..."
        ssh_cmd "$host" "mkdir -p /tmp/burst-build"
        scp_cmd -r "$PROJECT_DIR/" "${SSH_USER}@${host}:/tmp/burst-build/"
        ssh_cmd "$host" "cd /tmp/burst-build && docker build -t burst-daemon:testnet ."
        local image="burst-daemon:testnet"
    fi

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

    docker_args+=("$image")

    ssh_cmd "$host" "${docker_args[*]}"
    echo "  Node ${node_num} deployed (container: ${container_name})"
}

echo "=== Deploying seed node ==="
deploy_node "$SEED_IP" 1 "true"

echo ""
echo "Waiting 5s for seed node to initialize..."
sleep 5

node_num=2
for ip in "${NODE_IPS[@]}"; do
    echo ""
    echo "=== Deploying node ${node_num} ==="
    deploy_node "$ip" "$node_num" "false"
    node_num=$((node_num + 1))
done

echo ""
echo "=== Deployment Complete ==="
echo ""
echo "Seed node RPC:       http://${SEED_IP}:${RPC_PORT}"
echo "Seed node WebSocket: ws://${SEED_IP}:${WS_PORT}"
echo ""
echo "Check node health:"
echo "  ./deploy/health-check.sh ${ALL_IPS[*]}"
