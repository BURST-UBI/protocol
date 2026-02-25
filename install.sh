#!/bin/sh
# BURST Node — universal installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh
#
# Uninstall:
#   curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh -s -- --uninstall
#
# Environment variables (set before running):
#   BURST_NETWORK     — "test" (default) or "live"
#   BURST_SEED        — bootstrap peer address (default: testnet seed)
#   BURST_DATA_DIR    — data directory (default: /var/lib/burst)
#
# Works on any Linux with systemd (x86_64 or aarch64). The binary is
# fully static (musl) — zero runtime dependencies.

set -eu

REPO="BURST-UBI/protocol"
RELEASE_URL="https://github.com/${REPO}/releases/latest/download"
BIN_PATH="/usr/local/bin/burst-daemon"
CONFIG_DIR="/etc/burst"
CONFIG_PATH="${CONFIG_DIR}/config.toml"
DATA_DIR="${BURST_DATA_DIR:-/var/lib/burst}"
SERVICE_NAME="burst-node"
UPDATE_SERVICE="burst-update"
BURST_USER="burst"
BURST_NETWORK="${BURST_NETWORK:-test}"
BURST_SEED="${BURST_SEED:-167.172.83.88:17076}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { printf "${GREEN}[+]${NC} %s\n" "$1"; }
warn()  { printf "${YELLOW}[!]${NC} %s\n" "$1"; }
error() { printf "${RED}[x]${NC} %s\n" "$1"; exit 1; }

# ── Uninstall ────────────────────────────────────────────────────────

uninstall() {
    info "Stopping BURST services..."
    systemctl stop "${SERVICE_NAME}" 2>/dev/null || true
    systemctl stop "${UPDATE_SERVICE}.timer" 2>/dev/null || true
    systemctl disable "${SERVICE_NAME}" 2>/dev/null || true
    systemctl disable "${UPDATE_SERVICE}.timer" 2>/dev/null || true

    info "Removing systemd units..."
    rm -f "/etc/systemd/system/${SERVICE_NAME}.service"
    rm -f "/etc/systemd/system/${UPDATE_SERVICE}.service"
    rm -f "/etc/systemd/system/${UPDATE_SERVICE}.timer"
    systemctl daemon-reload

    info "Removing binary..."
    rm -f "${BIN_PATH}"

    info "Removing config..."
    rm -rf "${CONFIG_DIR}"

    printf "${YELLOW}[!]${NC} Data directory ${DATA_DIR} was NOT removed.\n"
    printf "    Remove it manually if you want:  sudo rm -rf %s\n" "${DATA_DIR}"
    printf "    Remove the burst user:           sudo userdel burst\n"

    info "BURST node uninstalled."
    exit 0
}

# ── Parse arguments ──────────────────────────────────────────────────

for arg in "$@"; do
    case "$arg" in
        --uninstall) uninstall ;;
        *) ;;
    esac
done

# ── Preflight checks ────────────────────────────────────────────────

[ "$(id -u)" -eq 0 ] || error "This script must be run as root (use sudo)."

command -v systemctl >/dev/null 2>&1 || error "systemd is required but not found."

ARCH="$(uname -m)"
case "${ARCH}" in
    x86_64)  ARCH_LABEL="amd64" ;;
    aarch64) ARCH_LABEL="arm64" ;;
    *)       error "Unsupported architecture: ${ARCH}. Only x86_64 and aarch64 are supported." ;;
esac

command -v curl >/dev/null 2>&1 || {
    warn "curl not found — installing..."
    if command -v apt-get >/dev/null 2>&1; then
        apt-get update -qq && apt-get install -y -qq curl >/dev/null
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y -q curl
    elif command -v apk >/dev/null 2>&1; then
        apk add --no-cache curl
    elif command -v pacman >/dev/null 2>&1; then
        pacman -Sy --noconfirm curl
    else
        error "Cannot install curl automatically. Please install curl and re-run."
    fi
}

info "Architecture: ${ARCH} (${ARCH_LABEL})"
info "Network: ${BURST_NETWORK}"

# ── Download binary ──────────────────────────────────────────────────

BINARY_URL="${RELEASE_URL}/burst-daemon-linux-${ARCH_LABEL}"

info "Downloading burst-daemon..."
if curl -fSL --progress-bar -o "${BIN_PATH}.tmp" "${BINARY_URL}"; then
    mv "${BIN_PATH}.tmp" "${BIN_PATH}"
    chmod +x "${BIN_PATH}"
    info "Installed ${BIN_PATH}"
else
    rm -f "${BIN_PATH}.tmp"
    error "Download failed. Is there a release at ${BINARY_URL} ?"
fi

# ── Create user and directories ──────────────────────────────────────

if ! id "${BURST_USER}" >/dev/null 2>&1; then
    useradd --system --no-create-home --home-dir "${DATA_DIR}" --shell /usr/sbin/nologin "${BURST_USER}"
    info "Created system user: ${BURST_USER}"
fi

mkdir -p "${DATA_DIR}"
chown "${BURST_USER}:${BURST_USER}" "${DATA_DIR}"

# ── Write config (skip if exists — preserves user edits) ─────────────

mkdir -p "${CONFIG_DIR}"

if [ ! -f "${CONFIG_PATH}" ]; then
    cat > "${CONFIG_PATH}" <<TOML
network = "Test"
data_dir = "${DATA_DIR}"
port = 17076
max_peers = 50
enable_rpc = true
rpc_port = 7077
enable_websocket = true
websocket_port = 7078
bootstrap_peers = ["${BURST_SEED}"]
log_format = "human"
log_level = "info"
work_threads = 2
enable_metrics = true
enable_faucet = false
enable_upnp = true
enable_verification = false
TOML
    info "Config written to ${CONFIG_PATH}"
else
    warn "Config already exists at ${CONFIG_PATH} — not overwriting."
fi

# ── Systemd service ──────────────────────────────────────────────────

cat > "/etc/systemd/system/${SERVICE_NAME}.service" <<EOF
[Unit]
Description=BURST Protocol Node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${BURST_USER}
ExecStart=${BIN_PATH} --config ${CONFIG_PATH} --data-dir ${DATA_DIR} node run
WorkingDirectory=${DATA_DIR}
Restart=always
RestartSec=5
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF

info "Installed ${SERVICE_NAME}.service"

# ── Auto-update service + timer ──────────────────────────────────────

cat > "/etc/systemd/system/${UPDATE_SERVICE}.service" <<EOF
[Unit]
Description=BURST Node Auto-Updater
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/bin/sh -c '\
    ARCH=\$(uname -m); \
    case "\$ARCH" in x86_64) A=amd64;; aarch64) A=arm64;; *) exit 0;; esac; \
    URL="https://github.com/${REPO}/releases/latest/download/burst-daemon-linux-\$A"; \
    NEW=\$(curl -fsSL "\$URL.sha256" 2>/dev/null || curl -fsSL "https://github.com/${REPO}/releases/latest/download/SHA256SUMS" 2>/dev/null | grep "\$A" | cut -d" " -f1); \
    OLD=\$(sha256sum ${BIN_PATH} 2>/dev/null | cut -d" " -f1); \
    if [ -n "\$NEW" ] && [ "\$NEW" != "\$OLD" ]; then \
        curl -fsSL -o ${BIN_PATH}.tmp "\$URL" && \
        VERIFY=\$(sha256sum ${BIN_PATH}.tmp | cut -d" " -f1) && \
        if [ "\$VERIFY" = "\$NEW" ]; then \
            mv ${BIN_PATH}.tmp ${BIN_PATH} && \
            chmod +x ${BIN_PATH} && \
            systemctl restart ${SERVICE_NAME}; \
        else \
            rm -f ${BIN_PATH}.tmp; \
        fi; \
    fi'
EOF

cat > "/etc/systemd/system/${UPDATE_SERVICE}.timer" <<EOF
[Unit]
Description=Check for BURST node updates every 5 minutes

[Timer]
OnBootSec=60
OnUnitActiveSec=300
RandomizedDelaySec=30

[Install]
WantedBy=timers.target
EOF

info "Installed auto-update timer (checks every 5 minutes)"

# ── Enable and start ─────────────────────────────────────────────────

systemctl daemon-reload
systemctl enable "${SERVICE_NAME}.service" >/dev/null 2>&1
systemctl enable "${UPDATE_SERVICE}.timer" >/dev/null 2>&1
systemctl restart "${SERVICE_NAME}.service"
systemctl start "${UPDATE_SERVICE}.timer"

info "Services started."

# ── Summary ──────────────────────────────────────────────────────────

echo ""
printf "${BOLD}════════════════════════════════════════════════${NC}\n"
printf "${BOLD}  BURST node installed successfully${NC}\n"
printf "${BOLD}════════════════════════════════════════════════${NC}\n"
echo ""
printf "  Binary:    ${BIN_PATH}\n"
printf "  Config:    ${CONFIG_PATH}\n"
printf "  Data:      ${DATA_DIR}\n"
printf "  Network:   ${BURST_NETWORK}\n"
printf "  P2P port:  17076\n"
printf "  RPC port:  7077\n"
echo ""
printf "  ${CYAN}View logs:${NC}     journalctl -u ${SERVICE_NAME} -f\n"
printf "  ${CYAN}Node status:${NC}   systemctl status ${SERVICE_NAME}\n"
printf "  ${CYAN}Restart:${NC}       sudo systemctl restart ${SERVICE_NAME}\n"
printf "  ${CYAN}Stop:${NC}          sudo systemctl stop ${SERVICE_NAME}\n"
printf "  ${CYAN}Uninstall:${NC}     curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sudo sh -s -- --uninstall\n"
echo ""
printf "  Auto-updates are enabled (checks every 5 minutes).\n"
printf "  Edit ${CONFIG_PATH} to customize your node.\n"
echo ""
