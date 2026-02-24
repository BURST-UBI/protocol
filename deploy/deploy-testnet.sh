#!/usr/bin/env bash
set -uo pipefail

# deploy-testnet.sh — Idempotent, resumable BURST testnet deployment.
#
# Safe to re-run at any time. Each node goes through discrete stages
# and only does work that hasn't already succeeded. Failed nodes don't
# block the rest — just re-run and it picks up where it left off.
#
# All output (build logs, errors) is saved per-node in ~/.burst-deploy/logs/.
# Errors are shown inline so you can see what went wrong immediately.
#
# Usage:
#   ./deploy/deploy-testnet.sh [flags] <seed-ip> [node-ip ...]
#   ./deploy/deploy-testnet.sh [flags] --nodes-file deploy/testnet-nodes.txt
#
# Flags:
#   --force           Force rebuild even if image is current
#   --nodes-file FILE Read IPs from file (one per line, # comments ok)
#
# Environment variables:
#   SSH_USER          — remote user (default: root)
#   SSH_PASS          — SSH password (uses sshpass; omit for key auth)
#   SSH_KEY           — path to SSH private key (default: ~/.ssh/id_ed25519)
#   BURST_REPO        — git repo URL (default: https://github.com/BURST-UBI/protocol.git)
#   BURST_BRANCH      — branch to deploy (default: main)
#   BURST_LOG_LEVEL   — log level for all nodes (default: info)

SSH_USER="${SSH_USER:-root}"
SSH_PASS="${SSH_PASS:-}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
BURST_REPO="${BURST_REPO:-https://github.com/BURST-UBI/protocol.git}"
BURST_BRANCH="${BURST_BRANCH:-main}"
BURST_LOG_LEVEL="${BURST_LOG_LEVEL:-info}"

FORCE_REBUILD=false

REPO_DIR="/opt/burst-protocol"
STATE_DIR="${HOME}/.burst-deploy"
LOG_DIR="${STATE_DIR}/logs"

P2P_PORT=17076
RPC_PORT=7077
WS_PORT=7078

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# ── Parse flags ──────────────────────────────────────────────────────
NODES_FILE=""
POSITIONAL=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --force)        FORCE_REBUILD=true; shift ;;
        --nodes-file)   NODES_FILE="$2"; shift 2 ;;
        -*)             echo "Unknown flag: $1"; exit 1 ;;
        *)              POSITIONAL+=("$1"); shift ;;
    esac
done

if [ -n "$NODES_FILE" ]; then
    if [ ! -f "$NODES_FILE" ]; then
        echo "Error: nodes file not found: $NODES_FILE"
        exit 1
    fi
    while IFS= read -r line; do
        line="${line%%#*}"
        line="$(echo "$line" | xargs)"
        [ -n "$line" ] && POSITIONAL+=("$line")
    done < "$NODES_FILE"
fi

if [ ${#POSITIONAL[@]} -lt 1 ]; then
    echo "Usage: $0 [--force] [--nodes-file FILE] <seed-ip> [node-ip ...]"
    echo ""
    echo "Examples:"
    echo "  SSH_PASS='pw' $0 167.172.83.88 159.65.80.231 143.244.131.5"
    echo "  SSH_PASS='pw' $0 --nodes-file deploy/testnet-nodes.txt"
    echo "  SSH_PASS='pw' $0 --force --nodes-file deploy/testnet-nodes.txt"
    exit 1
fi

SEED_IP="${POSITIONAL[0]}"
NODE_IPS=("${POSITIONAL[@]:1}")
ALL_IPS=("${POSITIONAL[@]}")

# ── SSH setup ────────────────────────────────────────────────────────
if [ -n "$SSH_PASS" ]; then
    if ! command -v sshpass &>/dev/null; then
        echo "Error: sshpass is required for password auth. Install it:"
        echo "  Arch:   sudo pacman -S sshpass"
        echo "  Ubuntu: sudo apt install sshpass"
        exit 1
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

# ── Logging ──────────────────────────────────────────────────────────
mkdir -p "$STATE_DIR" "$LOG_DIR"

node_log() {
    local host="$1"
    echo "${LOG_DIR}/${host}.log"
}

log_header() {
    local host="$1" step="$2"
    local logfile
    logfile=$(node_log "$host")
    echo "" >> "$logfile"
    echo "════════════════════════════════════════════════════════════" >> "$logfile"
    echo "  ${step}  —  $(date '+%Y-%m-%d %H:%M:%S')" >> "$logfile"
    echo "════════════════════════════════════════════════════════════" >> "$logfile"
}

show_error() {
    local host="$1" context="$2"
    local logfile
    logfile=$(node_log "$host")

    echo -e "  ${RED}${BOLD}ERROR${NC} during ${context}"
    echo -e "  ${YELLOW}Last 15 lines from log:${NC}"
    tail -15 "$logfile" 2>/dev/null | sed "s/^/    ${RED}│${NC} /"
    echo -e "  ${YELLOW}Full log: ${logfile}${NC}"
}

# ── State tracking ───────────────────────────────────────────────────

mark_stage() {
    local host="$1" stage="$2"
    echo "$(date -Iseconds)" > "${STATE_DIR}/${host}.${stage}"
}

has_stage() {
    local host="$1" stage="$2"
    [ -f "${STATE_DIR}/${host}.${stage}" ]
}

clear_stages() {
    local host="$1"
    rm -f "${STATE_DIR}/${host}".* 2>/dev/null || true
}

get_target_commit() {
    git ls-remote "$BURST_REPO" "refs/heads/${BURST_BRANCH}" 2>/dev/null | awk '{print $1}'
}

# ── Per-node deployment steps (each is idempotent) ───────────────────

step_check_ssh() {
    local host="$1"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [1/6] SSH connectivity... "
    log_header "$host" "SSH Check"

    local output
    output=$(ssh_cmd "$host" "echo ok; uname -a; free -h | head -2; df -h / | tail -1" 2>&1)
    local rc=$?
    echo "$output" >> "$logfile"

    if [ $rc -eq 0 ]; then
        echo -e "${GREEN}OK${NC}"
        return 0
    else
        echo -e "${RED}FAILED${NC}"
        show_error "$host" "SSH connection"
        return 1
    fi
}

step_install_deps() {
    local host="$1"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [2/6] Dependencies (git, docker)... "
    log_header "$host" "Install Dependencies"

    local needs_install
    needs_install=$(ssh_cmd "$host" "
        missing=''
        command -v git  &>/dev/null || missing=\"\${missing} git\"
        command -v docker &>/dev/null || missing=\"\${missing} docker\"
        echo \"\$missing\"
    " 2>&1)
    echo "Missing: [${needs_install}]" >> "$logfile"

    if [ -z "$(echo "$needs_install" | xargs)" ]; then
        echo -e "${GREEN}already installed${NC}"
        return 0
    fi

    echo -e "${CYAN}installing${needs_install}...${NC}"

    local output
    output=$(ssh_cmd "$host" "
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq git ca-certificates curl gnupg lsb-release

        if ! command -v docker &>/dev/null; then
            install -m 0755 -d /etc/apt/keyrings
            curl -fsSL https://download.docker.com/linux/\$(. /etc/os-release && echo \"\$ID\")/gpg \
                | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
            chmod a+r /etc/apt/keyrings/docker.gpg
            echo \"deb [arch=\$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] \
                https://download.docker.com/linux/\$(. /etc/os-release && echo \"\$ID\") \
                \$(lsb_release -cs) stable\" \
                | tee /etc/apt/sources.list.d/docker.list > /dev/null
            apt-get update -qq
            apt-get install -y -qq docker-ce docker-ce-cli containerd.io
            systemctl enable --now docker
        fi

        echo '--- versions ---'
        git --version
        docker --version
    " 2>&1)
    local rc=$?
    echo "$output" >> "$logfile"

    if [ $rc -eq 0 ]; then
        echo -e "  ${GREEN}Dependencies installed${NC}"
        return 0
    else
        show_error "$host" "dependency installation"
        return 1
    fi
}

step_sync_repo() {
    local host="$1" target_commit="$2"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [3/6] Git repo... "
    log_header "$host" "Sync Repo (target: ${target_commit:0:8})"

    local node_commit
    node_commit=$(ssh_cmd "$host" "
        if [ -d '${REPO_DIR}/.git' ]; then
            cd '${REPO_DIR}'
            git rev-parse HEAD
        else
            echo 'none'
        fi
    " 2>/dev/null)
    echo "Current commit on node: ${node_commit}" >> "$logfile"

    if [ "$node_commit" = "$target_commit" ]; then
        echo -e "${GREEN}up to date${NC} (${target_commit:0:8})"
        return 0
    fi

    local output rc
    if [ "$node_commit" = "none" ]; then
        echo -e "${CYAN}cloning...${NC}"
        output=$(ssh_cmd "$host" "git clone --branch '${BURST_BRANCH}' --depth 1 '${BURST_REPO}' '${REPO_DIR}'" 2>&1)
        rc=$?
    else
        echo -e "${CYAN}updating ${node_commit:0:8} -> ${target_commit:0:8}...${NC}"
        output=$(ssh_cmd "$host" "cd '${REPO_DIR}' && git fetch origin '${BURST_BRANCH}' && git reset --hard 'origin/${BURST_BRANCH}'" 2>&1)
        rc=$?
    fi
    echo "$output" >> "$logfile"

    if [ $rc -eq 0 ]; then
        echo -e "  ${GREEN}Repo synced${NC} to ${target_commit:0:8}"
        return 0
    else
        show_error "$host" "git sync"
        return 1
    fi
}

step_build_image() {
    local host="$1" target_commit="$2"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [4/6] Docker image... "
    log_header "$host" "Build Image (target: ${target_commit:0:8})"

    if [ "$FORCE_REBUILD" = "false" ]; then
        local image_commit
        image_commit=$(ssh_cmd "$host" "docker image inspect burst-daemon:testnet --format '{{index .Config.Labels \"burst.commit\"}}' 2>/dev/null || echo 'none'")
        echo "Current image commit: ${image_commit}" >> "$logfile"

        if [ "$image_commit" = "$target_commit" ]; then
            echo -e "${GREEN}already built${NC} for ${target_commit:0:8}"
            return 0
        fi
    fi

    echo -e "${CYAN}building (this takes a few minutes)...${NC}"
    local build_start
    build_start=$(date +%s)

    local output
    output=$(ssh_cmd "$host" "cd '${REPO_DIR}' && docker build --label 'burst.commit=${target_commit}' -t burst-daemon:testnet . 2>&1")
    local rc=$?
    echo "$output" >> "$logfile"

    local build_end elapsed
    build_end=$(date +%s)
    elapsed=$((build_end - build_start))

    if [ $rc -eq 0 ]; then
        echo -e "  ${GREEN}Image built${NC} (${elapsed}s)"
        clear_stages "$host"
        mark_stage "$host" "needs_restart"
        return 0
    else
        echo -e "  ${RED}${BOLD}BUILD FAILED${NC} after ${elapsed}s"
        echo ""
        echo -e "  ${RED}═══ Build errors ═══${NC}"
        # Show last 30 lines — enough to see the actual compile error
        echo "$output" | tail -30 | sed "s/^/  ${RED}│${NC} /"
        echo -e "  ${RED}═══════════════════${NC}"
        echo -e "  ${YELLOW}Full build log: ${logfile}${NC}"
        echo ""
        return 1
    fi
}

step_run_container() {
    local host="$1" node_num="$2" is_seed="$3"
    local container_name="burst-testnet-${node_num}"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [5/6] Container... "
    log_header "$host" "Run Container (${container_name})"

    local container_status
    container_status=$(ssh_cmd "$host" "
        running=\$(docker ps -q -f name='^${container_name}\$' 2>/dev/null)
        exists=\$(docker ps -aq -f name='^${container_name}\$' 2>/dev/null)
        if [ -n \"\$running\" ]; then
            echo 'running'
        elif [ -n \"\$exists\" ]; then
            echo 'stopped'
        else
            echo 'absent'
        fi
    ")
    echo "Container status: ${container_status}" >> "$logfile"

    local needs_restart=false
    if has_stage "$host" "needs_restart"; then
        needs_restart=true
        rm -f "${STATE_DIR}/${host}.needs_restart"
    fi

    if [ "$container_status" = "running" ] && [ "$needs_restart" = "false" ]; then
        echo -e "${GREEN}already running${NC}"
        return 0
    fi

    if [ "$container_status" != "absent" ]; then
        echo -e "${CYAN}replacing...${NC}"
        ssh_cmd "$host" "docker stop $container_name 2>/dev/null || true; docker rm $container_name 2>/dev/null || true" >> "$logfile" 2>&1
    else
        echo -e "${CYAN}starting...${NC}"
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
    echo "Running: ${docker_args[*]}" >> "$logfile"

    local output
    output=$(ssh_cmd "$host" "${docker_args[*]}" 2>&1)
    local rc=$?
    echo "$output" >> "$logfile"

    if [ $rc -eq 0 ]; then
        echo -e "  ${GREEN}Container started${NC}"
        return 0
    else
        show_error "$host" "container start"
        return 1
    fi
}

step_verify() {
    local host="$1" node_num="$2"
    local container_name="burst-testnet-${node_num}"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [6/6] Verify... "
    log_header "$host" "Verify"

    sleep 3

    local running
    running=$(ssh_cmd "$host" "docker ps -q -f name='^${container_name}\$' 2>/dev/null || true")

    if [ -n "$running" ]; then
        # Grab startup logs to check for immediate crashes
        local startup_logs
        startup_logs=$(ssh_cmd "$host" "docker logs --tail 20 $container_name 2>&1" 2>/dev/null || true)
        echo "$startup_logs" >> "$logfile"

        # Check if there are panic/error lines in startup
        local error_lines
        error_lines=$(echo "$startup_logs" | grep -iE "panic|fatal|error|failed to" || true)

        if [ -n "$error_lines" ]; then
            echo -e "${YELLOW}RUNNING (with warnings)${NC}"
            echo -e "  ${YELLOW}Suspicious lines in startup log:${NC}"
            echo "$error_lines" | head -5 | sed "s/^/    ${YELLOW}│${NC} /"
            echo -e "  ${YELLOW}Full log: ${logfile}${NC}"
            mark_stage "$host" "deployed"
            return 0
        fi

        echo -e "${GREEN}${BOLD}OK${NC} — healthy"
        mark_stage "$host" "deployed"
        return 0
    else
        echo -e "${RED}${BOLD}FAILED${NC} — container exited"
        echo ""

        local exit_logs
        exit_logs=$(ssh_cmd "$host" "docker logs --tail 30 $container_name 2>&1" 2>/dev/null || true)
        echo "$exit_logs" >> "$logfile"

        echo -e "  ${RED}═══ Container crash log ═══${NC}"
        echo "$exit_logs" | tail -20 | sed "s/^/  ${RED}│${NC} /"
        echo -e "  ${RED}═══════════════════════════${NC}"
        echo -e "  ${YELLOW}Full log: ${logfile}${NC}"
        echo ""
        return 1
    fi
}

# ── Deploy a single node (all steps, skip what's done) ───────────────

deploy_node() {
    local host="$1" node_num="$2" is_seed="$3" target_commit="$4"
    local logfile
    logfile=$(node_log "$host")

    # Start fresh log section for this run
    echo "" >> "$logfile"
    echo "################################################################" >> "$logfile"
    echo "# DEPLOYMENT RUN — $(date '+%Y-%m-%d %H:%M:%S')" >> "$logfile"
    echo "# Target: ${target_commit}" >> "$logfile"
    echo "# Node: ${node_num} ($([ "$is_seed" = "true" ] && echo "SEED" || echo "regular"))" >> "$logfile"
    echo "################################################################" >> "$logfile"

    echo ""
    if [ "$is_seed" = "true" ]; then
        echo -e "${BOLD}=== Node ${node_num}: ${host} (SEED) ===${NC}"
    else
        echo -e "${BOLD}=== Node ${node_num}: ${host} ===${NC}"
    fi

    step_check_ssh "$host" || {
        echo -e "  ${RED}SKIPPED — cannot reach host${NC}"
        return 1
    }
    step_install_deps "$host" || {
        echo -e "  ${RED}STOPPED — dependency install failed${NC}"
        return 1
    }
    step_sync_repo "$host" "$target_commit" || {
        echo -e "  ${RED}STOPPED — repo sync failed${NC}"
        return 1
    }
    step_build_image "$host" "$target_commit" || {
        echo -e "  ${RED}STOPPED — image build failed${NC}"
        return 1
    }
    step_run_container "$host" "$node_num" "$is_seed" || {
        echo -e "  ${RED}STOPPED — container start failed${NC}"
        return 1
    }
    step_verify "$host" "$node_num" || return 1

    return 0
}

# ── Main ─────────────────────────────────────────────────────────────

echo -e "${BOLD}=== BURST Testnet Deployment ===${NC}"
echo "Seed:   $SEED_IP"
echo "Nodes:  ${ALL_IPS[*]}"
echo "Repo:   $BURST_REPO ($BURST_BRANCH)"
echo "Force:  $FORCE_REBUILD"
echo "Logs:   $LOG_DIR"
echo ""

echo -n "Resolving target commit... "
TARGET_COMMIT=$(get_target_commit)
if [ -z "$TARGET_COMMIT" ]; then
    echo -e "${RED}FAILED — cannot reach $BURST_REPO${NC}"
    exit 1
fi
echo -e "${GREEN}${TARGET_COMMIT:0:8}${NC} (${BURST_BRANCH})"

SUCCEEDED=0
FAILED=0
FAILED_HOSTS=()

# Seed first
deploy_node "$SEED_IP" 1 "true" "$TARGET_COMMIT"
if [ $? -eq 0 ]; then
    SUCCEEDED=$((SUCCEEDED + 1))
else
    FAILED=$((FAILED + 1))
    FAILED_HOSTS+=("$SEED_IP")
fi

# Wait for seed before starting others
if [ ${#NODE_IPS[@]} -gt 0 ]; then
    echo ""
    echo "--- Waiting 5s for seed node ---"
    sleep 5
fi

node_num=2
for ip in "${NODE_IPS[@]}"; do
    deploy_node "$ip" "$node_num" "false" "$TARGET_COMMIT"
    if [ $? -eq 0 ]; then
        SUCCEEDED=$((SUCCEEDED + 1))
    else
        FAILED=$((FAILED + 1))
        FAILED_HOSTS+=("$ip")
    fi
    node_num=$((node_num + 1))
done

# ── Summary ──────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}═══════════════════════════════════${NC}"
if [ $FAILED -eq 0 ]; then
    echo -e "  ${GREEN}${BOLD}All ${SUCCEEDED}/${#ALL_IPS[@]} nodes deployed successfully${NC}"
else
    echo -e "  ${GREEN}Deployed: ${SUCCEEDED}/${#ALL_IPS[@]}${NC}"
    echo -e "  ${RED}Failed:   ${FAILED} — ${FAILED_HOSTS[*]}${NC}"
    echo ""
    echo "  Re-run the same command to retry failed nodes."
    echo "  Successful nodes will be skipped automatically."
fi
echo -e "${BOLD}═══════════════════════════════════${NC}"
echo ""
echo "Seed RPC:       http://${SEED_IP}:${RPC_PORT}"
echo "Seed WebSocket: ws://${SEED_IP}:${WS_PORT}"
echo ""
echo "Health check:   ./deploy/health-check.sh ${ALL_IPS[*]}"
echo "Auto-update:    ./deploy/update-testnet.sh ${ALL_IPS[*]}"
echo ""
echo -e "Per-node logs:  ${CYAN}ls ${LOG_DIR}/${NC}"
for ip in "${ALL_IPS[@]}"; do
    echo -e "  ${ip}: ${LOG_DIR}/${ip}.log"
done

if [ $FAILED -gt 0 ]; then
    exit 1
fi
