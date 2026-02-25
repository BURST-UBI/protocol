#!/usr/bin/env bash
set -uo pipefail

# deploy-testnet.sh — Idempotent, resumable BURST testnet deployment.
#
# Pulls pre-built Docker images from GHCR (built by CI). No compilation
# on the VPSes — just docker pull + run. Redeployments take seconds.
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
#   --force           Force re-pull + restart even if image is current
#   --nodes-file FILE Read IPs from file (one per line, # comments ok)
#   --tag TAG         Docker image tag to deploy (default: latest)
#
# Environment variables:
#   SSH_USER          — remote user (default: root)
#   SSH_PASS          — SSH password (uses sshpass; omit for key auth)
#   SSH_KEY           — path to SSH private key (default: ~/.ssh/id_ed25519)
#   BURST_LOG_LEVEL   — log level for all nodes (default: info)

SSH_USER="${SSH_USER:-root}"
SSH_PASS="${SSH_PASS:-}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519}"
BURST_LOG_LEVEL="${BURST_LOG_LEVEL:-info}"

FORCE_REBUILD=false
IMAGE_TAG="latest"

DOCKER_IMAGE="ghcr.io/burst-ubi/protocol"

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
        --tag)          IMAGE_TAG="$2"; shift 2 ;;
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
    echo "Usage: $0 [--force] [--nodes-file FILE] [--tag TAG] <seed-ip> [node-ip ...]"
    echo ""
    echo "Examples:"
    echo "  SSH_PASS='pw' $0 --nodes-file deploy/testnet-nodes.txt"
    echo "  SSH_PASS='pw' $0 --tag abc1234 --nodes-file deploy/testnet-nodes.txt"
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
        local arch
        arch=$(ssh_cmd "$host" "uname -m" 2>/dev/null | tr -d '[:space:]')
        echo -e "${GREEN}OK${NC} (${arch})"
        echo "$arch" > "${STATE_DIR}/${host}.arch"
        return 0
    else
        echo -e "${RED}FAILED${NC}"
        show_error "$host" "SSH connection"
        return 1
    fi
}

step_install_docker() {
    local host="$1"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [2/6] Docker... "
    log_header "$host" "Install Docker"

    local has_docker
    has_docker=$(ssh_cmd "$host" "command -v docker &>/dev/null && echo yes || echo no" 2>/dev/null)

    if [ "$has_docker" = "yes" ]; then
        echo -e "${GREEN}already installed${NC}"
        return 0
    fi

    echo -e "${CYAN}installing...${NC}"

    local output
    output=$(ssh_cmd "$host" "
        export DEBIAN_FRONTEND=noninteractive

        # Wait for any running apt/dpkg locks (unattended-upgrades, etc.)
        echo 'Waiting for apt locks...'
        while fuser /var/lib/dpkg/lock-frontend >/dev/null 2>&1 || \
              fuser /var/lib/apt/lists/lock >/dev/null 2>&1 || \
              fuser /var/cache/apt/archives/lock >/dev/null 2>&1; do
            echo '  apt is locked by another process, waiting 5s...'
            sleep 5
        done
        echo 'apt lock is free'

        apt-get update -qq
        apt-get install -y -qq ca-certificates curl gnupg lsb-release

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

        echo '--- version ---'
        docker --version
    " 2>&1)
    local rc=$?
    echo "$output" >> "$logfile"

    if [ $rc -eq 0 ]; then
        echo -e "  ${GREEN}Docker installed${NC}"
        return 0
    else
        show_error "$host" "Docker installation"
        return 1
    fi
}

step_pull_image() {
    local host="$1" tag="$2"
    local full_image="${DOCKER_IMAGE}:${tag}"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [3/6] Pull image... "
    log_header "$host" "Pull Image (${full_image})"

    if [ "$FORCE_REBUILD" = "false" ]; then
        local current_id
        current_id=$(ssh_cmd "$host" "docker image inspect '${full_image}' --format '{{.Id}}' 2>/dev/null || echo 'none'")

        # Pull to check for updates
        local pull_output
        pull_output=$(ssh_cmd "$host" "docker pull '${full_image}' 2>&1")
        local rc=$?
        echo "$pull_output" >> "$logfile"

        if [ $rc -ne 0 ]; then
            echo -e "${RED}FAILED${NC}"
            show_error "$host" "docker pull"
            return 1
        fi

        local new_id
        new_id=$(ssh_cmd "$host" "docker image inspect '${full_image}' --format '{{.Id}}' 2>/dev/null || echo 'none'")

        if [ "$current_id" = "$new_id" ] && [ "$current_id" != "none" ]; then
            echo -e "${GREEN}up to date${NC}"
            return 0
        fi

        echo -e "${GREEN}updated${NC}"
        mark_stage "$host" "needs_restart"
        return 0
    fi

    echo -e "${CYAN}pulling...${NC}"
    local output
    output=$(ssh_cmd "$host" "docker pull '${full_image}'" 2>&1)
    local rc=$?
    echo "$output" >> "$logfile"

    if [ $rc -eq 0 ]; then
        echo -e "  ${GREEN}Image pulled${NC}"
        mark_stage "$host" "needs_restart"
        return 0
    else
        show_error "$host" "docker pull"
        return 1
    fi
}

step_run_container() {
    local host="$1" node_num="$2" is_seed="$3"
    local container_name="burst-testnet-${node_num}"
    local full_image="${DOCKER_IMAGE}:${IMAGE_TAG}"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [4/6] Container... "
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

    docker_args+=("${full_image}")
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

step_watchtower() {
    local host="$1" node_num="$2"
    local container_name="burst-testnet-${node_num}"
    local logfile
    logfile=$(node_log "$host")

    echo -n "  [5/6] Watchtower (auto-update)... "
    log_header "$host" "Watchtower"

    local wt_status
    wt_status=$(ssh_cmd "$host" "
        running=\$(docker ps -q -f name='^watchtower\$' 2>/dev/null)
        if [ -n \"\$running\" ]; then
            echo 'running'
        else
            echo 'absent'
        fi
    ")

    if [ "$wt_status" = "running" ]; then
        echo -e "${GREEN}already running${NC}"
        return 0
    fi

    ssh_cmd "$host" "docker stop watchtower 2>/dev/null || true; docker rm watchtower 2>/dev/null || true" >> "$logfile" 2>&1

    # Watchtower is multi-arch (amd64, arm64, arm/v7) — docker pulls the
    # correct variant automatically. It watches only this node's container
    # and pulls the matching platform when a new multi-arch manifest appears.
    local output
    output=$(ssh_cmd "$host" "docker run -d \
        --name watchtower \
        --restart unless-stopped \
        -v /var/run/docker.sock:/var/run/docker.sock \
        containrrr/watchtower:latest \
        --interval 300 \
        --cleanup \
        --include-stopped \
        --revive-stopped \
        --label-enable=false \
        ${container_name}" 2>&1)
    local rc=$?
    echo "$output" >> "$logfile"

    if [ $rc -eq 0 ]; then
        echo -e "${GREEN}started${NC} (watches ${container_name}, polls every 5m)"
        return 0
    else
        echo -e "${YELLOW}SKIPPED${NC} — watchtower failed (non-critical)"
        return 0
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
        local startup_logs
        startup_logs=$(ssh_cmd "$host" "docker logs --tail 20 $container_name 2>&1" 2>/dev/null || true)
        echo "$startup_logs" >> "$logfile"

        local img_arch
        img_arch=$(ssh_cmd "$host" "docker inspect ${container_name} --format '{{.Architecture}}' 2>/dev/null || echo unknown" | tr -d '[:space:]')

        local error_lines
        error_lines=$(echo "$startup_logs" | grep -iE "panic|fatal|error|failed to" || true)

        if [ -n "$error_lines" ]; then
            echo -e "${YELLOW}RUNNING (with warnings)${NC} [${img_arch}]"
            echo -e "  ${YELLOW}Suspicious lines in startup log:${NC}"
            echo "$error_lines" | head -5 | sed "s/^/    ${YELLOW}│${NC} /"
            echo -e "  ${YELLOW}Full log: ${logfile}${NC}"
            mark_stage "$host" "deployed"
            return 0
        fi

        echo -e "${GREEN}${BOLD}OK${NC} — healthy [${img_arch}]"
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
    local host="$1" node_num="$2" is_seed="$3"
    local logfile
    logfile=$(node_log "$host")

    echo "" >> "$logfile"
    echo "################################################################" >> "$logfile"
    echo "# DEPLOYMENT RUN — $(date '+%Y-%m-%d %H:%M:%S')" >> "$logfile"
    echo "# Image: ${DOCKER_IMAGE}:${IMAGE_TAG}" >> "$logfile"
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
    step_install_docker "$host" || {
        echo -e "  ${RED}STOPPED — Docker install failed${NC}"
        return 1
    }
    step_pull_image "$host" "$IMAGE_TAG" || {
        echo -e "  ${RED}STOPPED — image pull failed${NC}"
        return 1
    }
    step_run_container "$host" "$node_num" "$is_seed" || {
        echo -e "  ${RED}STOPPED — container start failed${NC}"
        return 1
    }
    step_watchtower "$host" "$node_num"
    step_verify "$host" "$node_num" || return 1

    return 0
}

# ── Deploy a node silently (output to log + result file, for parallel use) ──

deploy_node_bg() {
    local host="$1" node_num="$2" is_seed="$3"
    local logfile result_file
    logfile=$(node_log "$host")
    result_file="${STATE_DIR}/${host}.result"

    {
        echo ""
        echo "################################################################"
        echo "# DEPLOYMENT RUN — $(date '+%Y-%m-%d %H:%M:%S')"
        echo "# Image: ${DOCKER_IMAGE}:${IMAGE_TAG}"
        echo "# Node: ${node_num} ($([ "$is_seed" = "true" ] && echo "SEED" || echo "regular"))"
        echo "################################################################"
        echo ""

        step_check_ssh "$host"                      || { echo "RESULT:FAIL:ssh"; echo "FAIL" > "$result_file"; return; }
        step_install_docker "$host"                  || { echo "RESULT:FAIL:docker"; echo "FAIL" > "$result_file"; return; }
        step_pull_image "$host" "$IMAGE_TAG"         || { echo "RESULT:FAIL:pull"; echo "FAIL" > "$result_file"; return; }
        step_run_container "$host" "$node_num" "$is_seed" || { echo "RESULT:FAIL:container"; echo "FAIL" > "$result_file"; return; }
        step_watchtower "$host" "$node_num"
        step_verify "$host" "$node_num"              || { echo "RESULT:FAIL:verify"; echo "FAIL" > "$result_file"; return; }

        echo "RESULT:OK"
        echo "OK" > "$result_file"
    } >> "$logfile" 2>&1
}

# ── Main ─────────────────────────────────────────────────────────────

echo -e "${BOLD}=== BURST Testnet Deployment ===${NC}"
echo "Seed:   $SEED_IP"
echo "Nodes:  ${ALL_IPS[*]}"
echo "Image:  ${DOCKER_IMAGE}:${IMAGE_TAG}"
echo "Force:  $FORCE_REBUILD"
echo "Logs:   $LOG_DIR"
echo ""

SUCCEEDED=0
FAILED=0
FAILED_HOSTS=()

# ── Step 1: Deploy seed node (must complete first) ───────────────────

deploy_node "$SEED_IP" 1 "true"
if [ $? -eq 0 ]; then
    SUCCEEDED=$((SUCCEEDED + 1))
else
    FAILED=$((FAILED + 1))
    FAILED_HOSTS+=("$SEED_IP")
    echo ""
    echo -e "${RED}${BOLD}Seed node failed — other nodes need the seed to bootstrap.${NC}"
    echo -e "${RED}Fix the seed first, then re-run.${NC}"
fi

# ── Step 2: Deploy remaining nodes in parallel ───────────────────────

if [ ${#NODE_IPS[@]} -gt 0 ] && [ $FAILED -eq 0 ]; then
    echo ""
    echo "--- Waiting 5s for seed node to initialize ---"
    sleep 5

    echo ""
    echo -e "${BOLD}=== Deploying nodes 2-$((${#NODE_IPS[@]} + 1)) in parallel ===${NC}"
    echo ""

    for ip in "${NODE_IPS[@]}"; do
        rm -f "${STATE_DIR}/${ip}.result"
    done

    PIDS=()
    node_num=2
    for ip in "${NODE_IPS[@]}"; do
        echo -e "  ${CYAN}Starting deploy for node ${node_num} (${ip})...${NC}"
        deploy_node_bg "$ip" "$node_num" "false" &
        PIDS+=($!)
        node_num=$((node_num + 1))
    done

    echo ""
    echo -e "  ${#PIDS[@]} deploys running in parallel. Waiting..."
    echo ""

    TOTAL_PARALLEL=${#NODE_IPS[@]}
    while true; do
        DONE=0
        for ip in "${NODE_IPS[@]}"; do
            [ -f "${STATE_DIR}/${ip}.result" ] && DONE=$((DONE + 1))
        done
        if [ $DONE -ge $TOTAL_PARALLEL ]; then
            break
        fi
        echo -ne "  \r  Progress: ${DONE}/${TOTAL_PARALLEL} nodes finished..."
        sleep 5
    done
    echo -e "\r  Progress: ${TOTAL_PARALLEL}/${TOTAL_PARALLEL} nodes finished.   "

    for pid in "${PIDS[@]}"; do
        wait "$pid" 2>/dev/null || true
    done

    echo ""
    node_num=2
    for ip in "${NODE_IPS[@]}"; do
        result="FAIL"
        [ -f "${STATE_DIR}/${ip}.result" ] && result=$(cat "${STATE_DIR}/${ip}.result")
        rm -f "${STATE_DIR}/${ip}.result"

        if [ "$result" = "OK" ]; then
            echo -e "  ${GREEN}✓ Node ${node_num} (${ip}) — deployed${NC}"
            SUCCEEDED=$((SUCCEEDED + 1))
        else
            fail_step=$(grep "RESULT:FAIL:" "$(node_log "$ip")" 2>/dev/null | tail -1 | cut -d: -f3)
            echo -e "  ${RED}✗ Node ${node_num} (${ip}) — failed at: ${fail_step:-unknown}${NC}"

            echo -e "    ${YELLOW}Last 10 lines from log:${NC}"
            tail -10 "$(node_log "$ip")" 2>/dev/null | grep -v "^RESULT:" | sed "s/^/    ${RED}│${NC} /"

            FAILED=$((FAILED + 1))
            FAILED_HOSTS+=("$ip")
        fi
        node_num=$((node_num + 1))
    done
fi

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

any_arch_saved=false
for ip in "${ALL_IPS[@]}"; do
    [ -f "${STATE_DIR}/${ip}.arch" ] && any_arch_saved=true && break
done
if [ "$any_arch_saved" = "true" ]; then
    echo "Node architectures:"
    for ip in "${ALL_IPS[@]}"; do
        arch="unknown"
        [ -f "${STATE_DIR}/${ip}.arch" ] && arch=$(cat "${STATE_DIR}/${ip}.arch")
        echo "  ${ip}: ${arch}"
    done
    echo ""
fi

echo "Health check:   ./deploy/health-check.sh ${ALL_IPS[*]}"
echo ""
echo -e "Per-node logs:  ${CYAN}ls ${LOG_DIR}/${NC}"
for ip in "${ALL_IPS[@]}"; do
    echo -e "  ${ip}: ${LOG_DIR}/${ip}.log"
done

if [ $FAILED -gt 0 ]; then
    exit 1
fi
