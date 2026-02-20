#!/usr/bin/env bash
set -euo pipefail

# setup-node.sh — Set up a BURST testnet node on a fresh Linux VPS.
#
# Run directly on the target VPS as root (or with sudo).
#
# Usage:
#   ./setup-node.sh [--seed] [--bootstrap-peers PEER1,PEER2]
#
# Options:
#   --seed              This node is the seed (enables RPC, WS, faucet publicly)
#   --bootstrap-peers   Comma-separated bootstrap peer addresses
#
# What this script does:
#   1. Installs Docker (if not present)
#   2. Configures firewall (ufw) for P2P, and optionally RPC/WS
#   3. Creates a systemd service for the BURST node container
#   4. Configures log rotation
#   5. Starts the service

IS_SEED=false
BOOTSTRAP_PEERS=""
P2P_PORT=17076
RPC_PORT=7077
WS_PORT=7078
DATA_DIR="/var/lib/burst"
CONTAINER_NAME="burst-testnet"

while [[ $# -gt 0 ]]; do
    case $1 in
        --seed)
            IS_SEED=true
            shift
            ;;
        --bootstrap-peers)
            BOOTSTRAP_PEERS="$2"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

echo "=== BURST Node Setup ==="
echo "Mode: $([ "$IS_SEED" = true ] && echo "SEED node" || echo "regular node")"
echo ""

# --- Install Docker ---
if ! command -v docker &>/dev/null; then
    echo "Installing Docker..."
    apt-get update -qq
    apt-get install -y -qq ca-certificates curl gnupg lsb-release

    install -m 0755 -d /etc/apt/keyrings
    curl -fsSL https://download.docker.com/linux/$(. /etc/os-release && echo "$ID")/gpg \
        | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
    chmod a+r /etc/apt/keyrings/docker.gpg

    echo \
      "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] \
      https://download.docker.com/linux/$(. /etc/os-release && echo "$ID") \
      $(lsb_release -cs) stable" \
      | tee /etc/apt/sources.list.d/docker.list > /dev/null

    apt-get update -qq
    apt-get install -y -qq docker-ce docker-ce-cli containerd.io docker-compose-plugin
    systemctl enable --now docker
    echo "Docker installed."
else
    echo "Docker already installed: $(docker --version)"
fi

# --- Firewall (ufw) ---
echo ""
echo "Configuring firewall..."
if command -v ufw &>/dev/null; then
    ufw --force enable 2>/dev/null || true
    ufw allow 22/tcp comment "SSH"
    ufw allow "${P2P_PORT}/tcp" comment "BURST P2P"

    if [ "$IS_SEED" = true ]; then
        ufw allow "${RPC_PORT}/tcp" comment "BURST RPC"
        ufw allow "${WS_PORT}/tcp" comment "BURST WebSocket"
    fi

    ufw reload
    echo "Firewall configured."
else
    echo "ufw not found — configure firewall manually:"
    echo "  Open TCP ports: 22 (SSH), ${P2P_PORT} (P2P)"
    if [ "$IS_SEED" = true ]; then
        echo "  Also open: ${RPC_PORT} (RPC), ${WS_PORT} (WebSocket)"
    fi
fi

# --- Data directory ---
mkdir -p "$DATA_DIR"
echo "Data directory: $DATA_DIR"

# --- Build environment variables for systemd ---
ENV_ARGS="-e BURST_NETWORK=test"
ENV_ARGS+=" -e BURST_LOG_LEVEL=info"
ENV_ARGS+=" -e BURST_ENABLE_WEBSOCKET=true"
ENV_ARGS+=" -e BURST_ENABLE_METRICS=true"

PORT_ARGS="-p ${P2P_PORT}:${P2P_PORT}"

if [ "$IS_SEED" = true ]; then
    ENV_ARGS+=" -e BURST_ENABLE_FAUCET=true"
    PORT_ARGS+=" -p ${RPC_PORT}:${RPC_PORT}"
    PORT_ARGS+=" -p ${WS_PORT}:${WS_PORT}"
fi

if [ -n "$BOOTSTRAP_PEERS" ]; then
    ENV_ARGS+=" -e BURST_BOOTSTRAP_PEERS=${BOOTSTRAP_PEERS}"
fi

# --- systemd service ---
echo ""
echo "Creating systemd service..."
cat > /etc/systemd/system/burst-node.service << UNIT
[Unit]
Description=BURST Testnet Node
After=docker.service
Requires=docker.service

[Service]
Type=simple
Restart=always
RestartSec=10
TimeoutStartSec=300

ExecStartPre=-/usr/bin/docker stop ${CONTAINER_NAME}
ExecStartPre=-/usr/bin/docker rm ${CONTAINER_NAME}

ExecStart=/usr/bin/docker run --rm \\
    --name ${CONTAINER_NAME} \\
    -v ${DATA_DIR}:/home/burst/data \\
    ${PORT_ARGS} \\
    ${ENV_ARGS} \\
    burst-daemon:testnet

ExecStop=/usr/bin/docker stop ${CONTAINER_NAME}

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable burst-node.service
echo "Systemd service created and enabled."

# --- Log rotation ---
echo ""
echo "Configuring log rotation..."
cat > /etc/logrotate.d/burst-node << 'LOGROTATE'
/var/log/burst-node.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    create 0640 root root
}
LOGROTATE

# Pipe container logs to a file for logrotate to manage
cat > /etc/cron.daily/burst-log-collect << 'CRON'
#!/bin/bash
docker logs --since 24h burst-testnet >> /var/log/burst-node.log 2>&1
CRON
chmod +x /etc/cron.daily/burst-log-collect
echo "Log rotation configured."

# --- Start ---
echo ""
echo "Starting BURST node..."
systemctl start burst-node.service

sleep 3
if systemctl is-active --quiet burst-node.service; then
    echo ""
    echo "=== BURST node is running ==="
    echo "Container: ${CONTAINER_NAME}"
    echo "Data:      ${DATA_DIR}"
    echo "P2P port:  ${P2P_PORT}"
    if [ "$IS_SEED" = true ]; then
        echo "RPC:       http://0.0.0.0:${RPC_PORT}"
        echo "WebSocket: ws://0.0.0.0:${WS_PORT}"
    fi
    echo ""
    echo "Commands:"
    echo "  systemctl status burst-node"
    echo "  journalctl -u burst-node -f"
    echo "  docker logs -f ${CONTAINER_NAME}"
else
    echo "WARNING: Service failed to start. Check:"
    echo "  systemctl status burst-node"
    echo "  journalctl -u burst-node --no-pager -n 50"
    exit 1
fi
