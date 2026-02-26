#!/bin/sh
# BURST Node — universal installer for Linux and macOS
#
# Linux:
#   curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh
#
# macOS:
#   curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sh
#
# Uninstall:
#   curl -fsSL https://raw.githubusercontent.com/BURST-UBI/protocol/main/install.sh | sudo sh -s -- --uninstall
#
# Environment variables (set before running):
#   BURST_NETWORK     — "test" (default) or "live"
#   BURST_SEED        — bootstrap peer address (default: testnet seed)
#   BURST_DATA_DIR    — data directory (default: platform-specific)

set -eu

REPO="BURST-UBI/protocol"
RELEASE_URL="https://github.com/${REPO}/releases/latest/download"
BURST_NETWORK="${BURST_NETWORK:-test}"
BURST_SEED="${BURST_SEED:-167.172.83.88:17076}"
# Set BURST_IS_SEED=1 when installing on the seed node itself (no bootstrap, faucet enabled).
BURST_IS_SEED="${BURST_IS_SEED:-0}"
# BURST_ADVERTISE_ADDRESS — optional override for keepalive self-advertisement.
# Node auto-detects public IP on cloud VPSes; set this only if auto-detect fails.
BURST_ADVERTISE_ADDRESS="${BURST_ADVERTISE_ADDRESS:-}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { printf "${GREEN}[+]${NC} %s\n" "$1"; }
warn()  { printf "${YELLOW}[!]${NC} %s\n" "$1"; }
error() { printf "${RED}[x]${NC} %s\n" "$1"; exit 1; }

# ── Detect platform ─────────────────────────────────────────────────

OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
    Linux)  PLATFORM="linux" ;;
    Darwin) PLATFORM="darwin" ;;
    *)      error "Unsupported OS: ${OS}. Only Linux and macOS are supported." ;;
esac

case "${ARCH}" in
    x86_64)  ARCH_LABEL="amd64" ;;
    aarch64) ARCH_LABEL="arm64" ;;
    arm64)   ARCH_LABEL="arm64" ;;
    *)       error "Unsupported architecture: ${ARCH}. Only x86_64 and arm64 are supported." ;;
esac

BINARY_URL="${RELEASE_URL}/burst-daemon-${PLATFORM}-${ARCH_LABEL}"

# ── Platform-specific paths ──────────────────────────────────────────

if [ "${PLATFORM}" = "linux" ]; then
    BIN_PATH="/usr/local/bin/burst-daemon"
    CONFIG_DIR="/etc/burst"
    CONFIG_PATH="${CONFIG_DIR}/config.toml"
    DATA_DIR="${BURST_DATA_DIR:-/var/lib/burst}"
    SERVICE_NAME="burst-node"
    UPDATE_SERVICE="burst-update"
    BURST_USER="burst"
else
    BURST_APP_DIR="${HOME}/Library/Application Support/BURST"
    BIN_DIR="${BURST_APP_DIR}/bin"
    BIN_PATH="${BIN_DIR}/burst-daemon"
    CONFIG_DIR="${BURST_APP_DIR}"
    CONFIG_PATH="${CONFIG_DIR}/config.toml"
    DATA_DIR="${BURST_DATA_DIR:-${BURST_APP_DIR}/data}"
    PLIST_DIR="${HOME}/Library/LaunchAgents"
    NODE_PLIST="com.burst.node"
    UPDATE_PLIST="com.burst.update"
fi

# ── Uninstall ────────────────────────────────────────────────────────

uninstall_linux() {
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

    info "Removing binary and config..."
    rm -f "${BIN_PATH}"
    rm -rf "${CONFIG_DIR}"

    printf "${YELLOW}[!]${NC} Data directory %s was NOT removed.\n" "${DATA_DIR}"
    printf "    Remove it manually:  sudo rm -rf %s\n" "${DATA_DIR}"
    printf "    Remove the user:     sudo userdel burst\n"
    info "BURST node uninstalled."
}

uninstall_macos() {
    info "Stopping BURST services..."
    launchctl bootout "gui/$(id -u)/${NODE_PLIST}" 2>/dev/null || true
    launchctl bootout "gui/$(id -u)/${UPDATE_PLIST}" 2>/dev/null || true

    info "Removing launchd plists..."
    rm -f "${PLIST_DIR}/${NODE_PLIST}.plist"
    rm -f "${PLIST_DIR}/${UPDATE_PLIST}.plist"

    info "Removing binary..."
    rm -f "${BIN_PATH}"
    rmdir "${BIN_DIR}" 2>/dev/null || true

    printf "${YELLOW}[!]${NC} Data and config at %s were NOT removed.\n" "${BURST_APP_DIR}"
    printf "    Remove manually:  rm -rf \"%s\"\n" "${BURST_APP_DIR}"
    info "BURST node uninstalled."
}

for arg in "$@"; do
    case "$arg" in
        --uninstall)
            if [ "${PLATFORM}" = "linux" ]; then uninstall_linux; else uninstall_macos; fi
            exit 0
            ;;
        *) ;;
    esac
done

# ── Preflight ────────────────────────────────────────────────────────

if [ "${PLATFORM}" = "linux" ]; then
    [ "$(id -u)" -eq 0 ] || error "This script must be run as root (use sudo)."
    command -v systemctl >/dev/null 2>&1 || error "systemd is required but not found."
fi

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
    elif command -v brew >/dev/null 2>&1; then
        brew install curl
    else
        error "Cannot install curl automatically. Please install curl and re-run."
    fi
}

info "Platform: ${PLATFORM}/${ARCH_LABEL}"
info "Network: ${BURST_NETWORK}"

# ── Download binary ──────────────────────────────────────────────────

info "Downloading burst-daemon..."

# macOS: create user-owned bin directory (no sudo needed for updates)
if [ "${PLATFORM}" = "darwin" ]; then
    mkdir -p "${BIN_DIR}"
fi

TMP_BIN="${BIN_PATH}.tmp"

if curl -fSL --progress-bar -o "${TMP_BIN}" "${BINARY_URL}"; then
    mv "${TMP_BIN}" "${BIN_PATH}"
    chmod +x "${BIN_PATH}"
    info "Installed ${BIN_PATH}"
else
    rm -f "${TMP_BIN}"
    error "Download failed. Is there a release at ${BINARY_URL} ?"
fi

# ── Write config (skip if exists) ────────────────────────────────────

mkdir -p "${CONFIG_DIR}"
mkdir -p "${DATA_DIR}"

if [ ! -f "${CONFIG_PATH}" ]; then
    if [ "${BURST_IS_SEED}" = "1" ] || [ -z "${BURST_SEED}" ]; then
        BOOTSTRAP_LINE="bootstrap_peers = []"
        FAUCET_LINE="enable_faucet = true"
        info "Installing as SEED node (no bootstrap, faucet enabled)"
    else
        BOOTSTRAP_LINE="bootstrap_peers = [\"${BURST_SEED}\"]"
        FAUCET_LINE="enable_faucet = false"
        info "Installing as NON-SEED node (bootstrap from ${BURST_SEED})"
    fi
    cat > "${CONFIG_PATH}" <<TOML
network = "Test"
data_dir = "${DATA_DIR}"
port = 17076
max_peers = 50
enable_rpc = true
rpc_port = 7077
enable_websocket = true
websocket_port = 7078
${BOOTSTRAP_LINE}
log_format = "human"
log_level = "info"
work_threads = 2
enable_metrics = true
${FAUCET_LINE}
enable_upnp = true
enable_verification = false
TOML
    if [ -n "${BURST_ADVERTISE_ADDRESS}" ]; then
        echo "advertise_address = \"${BURST_ADVERTISE_ADDRESS}\"" >> "${CONFIG_PATH}"
        info "Set advertise_address override (auto-detect disabled)"
    fi
    info "Config written to ${CONFIG_PATH}"
else
    warn "Config already exists at ${CONFIG_PATH} — not overwriting."
fi

# ══════════════════════════════════════════════════════════════════════
# Linux: systemd service + timer
# ══════════════════════════════════════════════════════════════════════

install_linux() {
    if ! id "${BURST_USER}" >/dev/null 2>&1; then
        useradd --system --no-create-home --home-dir "${DATA_DIR}" --shell /usr/sbin/nologin "${BURST_USER}"
        info "Created system user: ${BURST_USER}"
    fi
    chown "${BURST_USER}:${BURST_USER}" "${DATA_DIR}"

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
    NEW=\$(curl -fsSL "https://github.com/${REPO}/releases/latest/download/SHA256SUMS" 2>/dev/null | grep "linux-\$A" | cut -d" " -f1); \
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
Description=Check for BURST node updates every 30 minutes

[Timer]
OnBootSec=60
OnUnitActiveSec=1800
RandomizedDelaySec=60

[Install]
WantedBy=timers.target
EOF

    systemctl daemon-reload
    systemctl enable "${SERVICE_NAME}.service" >/dev/null 2>&1
    systemctl enable "${UPDATE_SERVICE}.timer" >/dev/null 2>&1
    systemctl restart "${SERVICE_NAME}.service"
    systemctl start "${UPDATE_SERVICE}.timer"

    info "Services started."
    echo ""
    printf "${BOLD}════════════════════════════════════════════════${NC}\n"
    printf "${BOLD}  BURST node installed successfully${NC}\n"
    printf "${BOLD}════════════════════════════════════════════════${NC}\n"
    echo ""
    printf "  Binary:    %s\n" "${BIN_PATH}"
    printf "  Config:    %s\n" "${CONFIG_PATH}"
    printf "  Data:      %s\n" "${DATA_DIR}"
    printf "  Network:   %s\n" "${BURST_NETWORK}"
    printf "  P2P port:  17076\n"
    printf "  RPC port:  7077\n"
    echo ""
    printf "  ${CYAN}View logs:${NC}     journalctl -u ${SERVICE_NAME} -f\n"
    printf "  ${CYAN}Node status:${NC}   systemctl status ${SERVICE_NAME}\n"
    printf "  ${CYAN}Restart:${NC}       sudo systemctl restart ${SERVICE_NAME}\n"
    printf "  ${CYAN}Stop:${NC}          sudo systemctl stop ${SERVICE_NAME}\n"
    printf "  ${CYAN}Uninstall:${NC}     curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sudo sh -s -- --uninstall\n"
    echo ""
    printf "  Auto-updates are enabled (checks every 30 minutes).\n"
    printf "  Edit %s to customize your node.\n" "${CONFIG_PATH}"
    echo ""
}

# ══════════════════════════════════════════════════════════════════════
# macOS: launchd plists
# ══════════════════════════════════════════════════════════════════════

install_macos() {
    mkdir -p "${PLIST_DIR}"

    cat > "${PLIST_DIR}/${NODE_PLIST}.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${NODE_PLIST}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${BIN_PATH}</string>
        <string>--config</string>
        <string>${CONFIG_PATH}</string>
        <string>--data-dir</string>
        <string>${DATA_DIR}</string>
        <string>node</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${BURST_APP_DIR}/burst-node.log</string>
    <key>StandardErrorPath</key>
    <string>${BURST_APP_DIR}/burst-node.log</string>
</dict>
</plist>
EOF

    cat > "${PLIST_DIR}/${UPDATE_PLIST}.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${UPDATE_PLIST}</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>-c</string>
        <string>
ARCH=\$(uname -m);
case "\$ARCH" in x86_64) A=amd64;; arm64) A=arm64;; *) exit 0;; esac;
URL="https://github.com/${REPO}/releases/latest/download/burst-daemon-darwin-\$A";
NEW=\$(curl -fsSL "https://github.com/${REPO}/releases/latest/download/SHA256SUMS" 2>/dev/null | grep "darwin-\$A" | cut -d" " -f1);
OLD=\$(shasum -a 256 "${BIN_PATH}" 2>/dev/null | cut -d" " -f1);
if [ -n "\$NEW" ] &amp;&amp; [ "\$NEW" != "\$OLD" ]; then
    curl -fsSL -o "${BIN_PATH}.tmp" "\$URL" &amp;&amp;
    VERIFY=\$(shasum -a 256 "${BIN_PATH}.tmp" | cut -d" " -f1) &amp;&amp;
    if [ "\$VERIFY" = "\$NEW" ]; then
        mv "${BIN_PATH}.tmp" "${BIN_PATH}" &amp;&amp;
        chmod +x "${BIN_PATH}" &amp;&amp;
        launchctl kickstart -k "gui/\$(id -u)/${NODE_PLIST}";
    else
        rm -f "${BIN_PATH}.tmp";
    fi;
fi
        </string>
    </array>
    <key>StartInterval</key>
    <integer>1800</integer>
    <key>StandardOutPath</key>
    <string>${BURST_APP_DIR}/burst-update.log</string>
    <key>StandardErrorPath</key>
    <string>${BURST_APP_DIR}/burst-update.log</string>
</dict>
</plist>
EOF

    launchctl bootout "gui/$(id -u)/${NODE_PLIST}" 2>/dev/null || true
    launchctl bootout "gui/$(id -u)/${UPDATE_PLIST}" 2>/dev/null || true
    launchctl bootstrap "gui/$(id -u)" "${PLIST_DIR}/${NODE_PLIST}.plist"
    launchctl bootstrap "gui/$(id -u)" "${PLIST_DIR}/${UPDATE_PLIST}.plist"

    info "Services started."
    echo ""
    printf "${BOLD}════════════════════════════════════════════════${NC}\n"
    printf "${BOLD}  BURST node installed successfully${NC}\n"
    printf "${BOLD}════════════════════════════════════════════════${NC}\n"
    echo ""
    printf "  Binary:    %s\n" "${BIN_PATH}"
    printf "  Config:    %s\n" "${CONFIG_PATH}"
    printf "  Data:      %s\n" "${DATA_DIR}"
    printf "  Logs:      %s/burst-node.log\n" "${BURST_APP_DIR}"
    printf "  Network:   %s\n" "${BURST_NETWORK}"
    printf "  P2P port:  17076\n"
    printf "  RPC port:  7077\n"
    echo ""
    printf "  ${CYAN}View logs:${NC}     tail -f \"%s/burst-node.log\"\n" "${BURST_APP_DIR}"
    printf "  ${CYAN}Restart:${NC}       launchctl kickstart -k gui/\$(id -u)/${NODE_PLIST}\n"
    printf "  ${CYAN}Stop:${NC}          launchctl kill SIGTERM gui/\$(id -u)/${NODE_PLIST}\n"
    printf "  ${CYAN}Uninstall:${NC}     curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sh -s -- --uninstall\n"
    echo ""
    printf "  Auto-updates are enabled (checks every 30 minutes).\n"
    printf "  Edit \"%s\" to customize your node.\n" "${CONFIG_PATH}"
    echo ""
}

# ── Run platform-specific installer ──────────────────────────────────

if [ "${PLATFORM}" = "linux" ]; then
    install_linux
else
    install_macos
fi
