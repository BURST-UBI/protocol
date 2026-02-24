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
# Each VPS clones/pulls from the GitHub repo and builds the Docker image
# locally. No source code is copied from your machine.
#
# Prerequisites:
#   - SSH access to all VPSes (password or key-based)
#   - Docker + git installed on all VPSes (or use deploy/setup-node.sh first)
#   - sshpass installed locally if using password auth
#
# Environment variables:
#   SSH_USER          — remote user (default: root)
#   SSH_PASS          — SSH password (if set, uses password auth via sshpass)
#   SSH_KEY           — path to SSH private key (default: ~/.ssh/id_ed25519, ignored if SSH_PASS set)
#   BURST_REPO        — git repo URL (default: https://github.com/BURST-UBI/protocol.git)
#   BURST_BRANCH      — branch to deploy (default: main)
#   BURST_LOG_LEVEL   — log level for all nodes (default: info)

SSH_USER="${SSH_USER:-root}"
SSH_PASS="${SSH_PASS:-}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
BURST_REPO="${BURST_REPO:-https://github.com/BURST-UBI/protocol.git}"
BURST_BRANCH="${BURST_BRANCH:-main}"
BURST_LOG_LEVEL="${BURST_LOG_LEVEL:-info}"

REPO_DIR="/opt/burst-protocol"

P2P_PORT=17076
RPC_PORT=7077
WS_PORT=7078

if [ -n "$SSH_PASS" ]; then
    if ! command -v sshpass &>/dev/null; then
        echo "Error: sshpass is required for password auth. Install it:"
        echo "  Arch:   sudo pacman -S sshpass"
        echo "  Ubuntu: sudo apt install sshpass"
        echo "  Mac:    brew install sshpass"
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

usage() {
    echo "Usage: $0 <seed-ip> [node-ip ...]"
    echo ""
    echo "Deploy a BURST testnet. First IP is the seed node."
    echo ""
    echo "Examples:"
    echo "  SSH_PASS='mypassword' $0 167.172.83.88 159.65.80.231 143.244.131.5"
    echo "  SSH_KEY=~/.ssh/id_ed25519 $0 10.0.0.1 10.0.0.2 10.0.0.3"
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
echo "Seed node:   $SEED_IP"
echo "Other nodes: ${NODE_IPS[*]:-none}"
echo "SSH user:    $SSH_USER"
echo "Repo:        $BURST_REPO"
echo "Branch:      $BURST_BRANCH"
echo ""

# Clone or pull the repo on a remote host. Returns "updated" if new
# changes were pulled, "fresh" on first clone, "current" if already
# up to date.
sync_repo() {
    local host="$1"
    echo "  Syncing repo on ${host}..."

    ssh_cmd "$host" "apt-get install -y -qq git 2>/dev/null || true"

    local status
    status=$(ssh_cmd "$host" "
        if [ -d '${REPO_DIR}/.git' ]; then
            cd '${REPO_DIR}'
            git fetch origin '${BURST_BRANCH}' --quiet
            LOCAL=\$(git rev-parse HEAD)
            REMOTE=\$(git rev-parse 'origin/${BURST_BRANCH}')
            if [ \"\$LOCAL\" = \"\$REMOTE\" ]; then
                echo 'current'
            else
                git reset --hard 'origin/${BURST_BRANCH}' --quiet
                echo 'updated'
            fi
        else
            git clone --branch '${BURST_BRANCH}' --depth 1 '${BURST_REPO}' '${REPO_DIR}' --quiet
            echo 'fresh'
        fi
    ")
    echo "  Repo status: ${status}"
    echo "$status"
}

build_image() {
    local host="$1"
    echo "  Building Docker image on ${host} (this may take a while)..."
    ssh_cmd "$host" "cd '${REPO_DIR}' && docker build -t burst-daemon:testnet . 2>&1 | tail -3"
}

deploy_node() {
    local host="$1"
    local node_num="$2"
    local is_seed="$3"
    local force_rebuild="$4"
    local container_name="burst-testnet-${node_num}"

    echo "--- Node ${node_num} (${host}) ---"

    local repo_status
    repo_status=$(sync_repo "$host")

    local needs_rebuild=false
    if [ "$repo_status" = "fresh" ] || [ "$repo_status" = "updated" ] || [ "$force_rebuild" = "true" ]; then
        needs_rebuild=true
    fi

    local image_exists
    image_exists=$(ssh_cmd "$host" "docker image inspect burst-daemon:testnet >/dev/null 2>&1 && echo yes || echo no")

    if [ "$image_exists" = "no" ]; then
        needs_rebuild=true
    fi

    if [ "$needs_rebuild" = "true" ]; then
        build_image "$host"

        ssh_cmd "$host" "docker stop $container_name 2>/dev/null || true"
        ssh_cmd "$host" "docker rm $container_name 2>/dev/null || true"
    else
        echo "  No changes — skipping rebuild"
        local running
        running=$(ssh_cmd "$host" "docker ps -q -f name=$container_name 2>/dev/null || true")
        if [ -n "$running" ]; then
            echo "  Container already running"
            return 0
        fi
        ssh_cmd "$host" "docker rm $container_name 2>/dev/null || true"
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

    docker_args+=("burst-daemon:testnet")

    ssh_cmd "$host" "${docker_args[*]}"
    echo "  Node ${node_num} deployed"
}

# --- Deploy all nodes ---
echo "=== Deploying seed node ==="
deploy_node "$SEED_IP" 1 "true" "false"

echo ""
echo "Waiting 5s for seed node to initialize..."
sleep 5

node_num=2
for ip in "${NODE_IPS[@]}"; do
    echo ""
    echo "=== Deploying node ${node_num} ==="
    deploy_node "$ip" "$node_num" "false" "false"
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
echo ""
echo "To auto-update when repo changes:"
echo "  ./deploy/update-testnet.sh ${ALL_IPS[*]}"
