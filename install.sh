#!/bin/sh
# BURST node installer for supported Linux and macOS targets
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
#   BURST_SEED        — bootstrap peer address (default: none)
#   BURST_DATA_DIR    — data directory (default: platform-specific)

set -eu

REPO="BURST-UBI/protocol"
RELEASE_URL="https://github.com/${REPO}/releases/latest/download"
BURST_NETWORK="${BURST_NETWORK:-test}"
BURST_SEED="${BURST_SEED:-}"
# Set BURST_IS_SEED=1 when installing a seed node (no bootstrap; no faucet).
BURST_IS_SEED="${BURST_IS_SEED:-0}"
# BURST_ADVERTISE_ADDRESS — optional override for keepalive self-advertisement.
# Auto-detection only finds a public IP directly assigned to the outbound interface.
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

download_verified_binary() (
    set -eu
    destination=$1
    asset=$2
    stage=$(mktemp -d "${destination}.download.XXXXXX") || exit 1
    trap 'rm -f "$stage/binary" "$stage/SHA256SUMS"; rmdir "$stage"' EXIT
    trap 'exit 1' HUP INT TERM
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 10 --max-time 180 --retry 2 \
        -o "$stage/SHA256SUMS" "${RELEASE_URL}/SHA256SUMS" || exit 1
    expected=$(awk -v asset="$asset" '
        $2 == asset {
            count++
            if (NF != 2 || length($1) != 64 || $1 ~ /[^[:xdigit:]]/) invalid = 1
            hash = tolower($1)
        }
        END { if (count != 1 || invalid) exit 1; print hash }
    ' "$stage/SHA256SUMS") || { echo 'Missing, duplicate or invalid release checksum' >&2; exit 1; }
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 10 --max-time 180 --retry 2 \
        -o "$stage/binary" "${RELEASE_URL}/${asset}" || exit 1
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$stage/binary" | awk '{ print $1 }')
    else
        actual=$(shasum -a 256 "$stage/binary" | awk '{ print $1 }')
    fi
    [ "$actual" = "$expected" ] || { echo 'Release checksum mismatch; installed binary was not changed' >&2; exit 1; }
    chmod 755 "$stage/binary" || exit 1
    mv -f "$stage/binary" "$destination"
)


xml_text() {
    printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g"
}

systemd_arg() {
    printf '"'
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' -e 's/%/%%/g'
    printf '"'
}

toml_string() {
    printf '"'
    printf '%s' "$1" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g'
    printf '"'
}

# Detect platform
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

# Platform-specific paths
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

# Reject control characters before embedding paths or endpoint values in config.
for value in "$DATA_DIR" "$BIN_PATH" "$CONFIG_PATH" "$BURST_SEED" "$BURST_ADVERTISE_ADDRESS"; do
    clean=$(printf '%s' "$value" | LC_ALL=C tr -d '[:cntrl:]')
    [ "$clean" = "$value" ] || error "Paths and endpoint settings cannot contain control characters"
done

# Uninstall
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
    rm -f "${BIN_PATH}" "${BIN_PATH}.updater"
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
    rm -f "${BIN_PATH}" "${BIN_PATH}.updater"
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

# Preflight
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

# Resolve network → config value (capitalized, matches NetworkId serde) and
# its P2P port, once, so both the config writer and the summary agree.
case "${BURST_NETWORK}" in
    live|Live|LIVE) NET_NAME="Live"; NET_PORT=7076 ;;
    dev|Dev|DEV)    NET_NAME="Dev";  NET_PORT=27076 ;;
    test|Test|TEST) NET_NAME="Test"; NET_PORT=17076 ;;
    *) error "BURST_NETWORK must be live, test or dev" ;;
esac

info "Platform: ${PLATFORM}/${ARCH_LABEL}"
info "Network: ${BURST_NETWORK} (${NET_NAME}, P2P ${NET_PORT})"

# Download binary
info "Downloading burst-daemon..."

# macOS: create user-owned bin directory (no sudo needed for updates)
if [ "${PLATFORM}" = "darwin" ]; then
    mkdir -p "${BIN_DIR}"
fi

if download_verified_binary "${BIN_PATH}" "burst-daemon-${PLATFORM}-${ARCH_LABEL}"; then
    info "Installed ${BIN_PATH}"
else
    error "Release download or checksum validation failed for ${BINARY_URL}"
fi

UPDATER_PATH="${BIN_PATH}.updater"
if ! download_verified_binary "${UPDATER_PATH}" "burst-update.sh"; then
    error "Could not install the verified updater"
fi

# Write config (skip if exists)
mkdir -p "${CONFIG_DIR}"
mkdir -p "${DATA_DIR}"

if [ ! -f "${CONFIG_PATH}" ]; then
    if [ "${BURST_IS_SEED}" = "1" ] || [ -z "${BURST_SEED}" ]; then
        BOOTSTRAP_LINE="bootstrap_peers = []"
        info "Installing as SEED node (no bootstrap peers)"
    else
        BOOTSTRAP_LINE="bootstrap_peers = [$(toml_string "${BURST_SEED}")]"
        info "Installing as NON-SEED node (bootstrap from ${BURST_SEED})"
    fi

    # Installing a node or omitting a seed never grants test-funding authority.
    FAUCET_LINE="enable_faucet = false"

    cat > "${CONFIG_PATH}" <<TOML
network = "${NET_NAME}"
data_dir = $(toml_string "${DATA_DIR}")
port = ${NET_PORT}
max_peers = 50
enable_rpc = true
rpc_port = 7077
# Bind RPC to localhost only — the RPC surface includes genesis_endorse.
# Set to "0.0.0.0" only behind a firewall that restricts this port.
rpc_bind = "127.0.0.1"
enable_websocket = true
websocket_port = 7078
websocket_bind = "127.0.0.1"
${BOOTSTRAP_LINE}
log_format = "human"
log_level = "info"
work_threads = 2
enable_metrics = true
${FAUCET_LINE}
enable_upnp = true
enable_verification = false
enable_representative = true
TOML
    if [ -n "${BURST_ADVERTISE_ADDRESS}" ]; then
        printf 'advertise_address = %s\n' "$(toml_string "${BURST_ADVERTISE_ADDRESS}")" >> "${CONFIG_PATH}"
        info "Set advertise_address override (auto-detect disabled)"
    fi
    info "Config written to ${CONFIG_PATH}"
else
    warn "Config already exists at ${CONFIG_PATH} — not overwriting."
fi

#
# Linux: systemd service + timer
#
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
ExecStart=:$(systemd_arg "${BIN_PATH}") --config $(systemd_arg "${CONFIG_PATH}") node run
WorkingDirectory=$(systemd_arg "${DATA_DIR}")
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
ExecStart=:/bin/sh $(systemd_arg "${UPDATER_PATH}") $(systemd_arg "${BIN_PATH}") ${PLATFORM}-${ARCH_LABEL} ${SERVICE_NAME}
EOF

    cat > "/etc/systemd/system/${UPDATE_SERVICE}.timer" <<EOF
[Unit]
Description=Check for BURST node updates every 5 minutes

[Timer]
OnBootSec=60
OnUnitActiveSec=300
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
    printf "  P2P port:  %s\n" "${NET_PORT:-17076}"
    printf "  RPC port:  7077\n"
    echo ""
    printf "  ${CYAN}View logs:${NC}     journalctl -u ${SERVICE_NAME} -f\n"
    printf "  ${CYAN}Node status:${NC}   systemctl status ${SERVICE_NAME}\n"
    printf "  ${CYAN}Restart:${NC}       sudo systemctl restart ${SERVICE_NAME}\n"
    printf "  ${CYAN}Stop:${NC}          sudo systemctl stop ${SERVICE_NAME}\n"
    printf "  ${CYAN}Uninstall:${NC}     curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sudo sh -s -- --uninstall\n"
    echo ""
    printf "  Auto-updates are enabled (checks every 5 minutes).\n"
    printf "  Edit %s to customize your node.\n" "${CONFIG_PATH}"
    echo ""
}

#
# macOS: launchd plists
#
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
        <string>$(xml_text "${BIN_PATH}")</string>
        <string>--config</string>
        <string>$(xml_text "${CONFIG_PATH}")</string>
        <string>node</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>$(xml_text "${BURST_APP_DIR}/burst-node.log")</string>
    <key>StandardErrorPath</key>
    <string>$(xml_text "${BURST_APP_DIR}/burst-node.log")</string>
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
        <string>$(xml_text "${UPDATER_PATH}")</string>
        <string>$(xml_text "${BIN_PATH}")</string>
        <string>${PLATFORM}-${ARCH_LABEL}</string>
        <string>${NODE_PLIST}</string>
    </array>
    <key>StartInterval</key>
    <integer>300</integer>
    <key>StandardOutPath</key>
    <string>$(xml_text "${BURST_APP_DIR}/burst-update.log")</string>
    <key>StandardErrorPath</key>
    <string>$(xml_text "${BURST_APP_DIR}/burst-update.log")</string>
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
    printf "  P2P port:  %s\n" "${NET_PORT:-17076}"
    printf "  RPC port:  7077\n"
    echo ""
    printf "  ${CYAN}View logs:${NC}     tail -f \"%s/burst-node.log\"\n" "${BURST_APP_DIR}"
    printf "  ${CYAN}Restart:${NC}       launchctl kickstart -k gui/\$(id -u)/${NODE_PLIST}\n"
    printf "  ${CYAN}Stop:${NC}          launchctl kill SIGTERM gui/\$(id -u)/${NODE_PLIST}\n"
    printf "  ${CYAN}Uninstall:${NC}     curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | sh -s -- --uninstall\n"
    echo ""
    printf "  Auto-updates are enabled (checks every 5 minutes).\n"
    printf "  Edit \"%s\" to customize your node.\n" "${CONFIG_PATH}"
    echo ""
}

# Run platform-specific installer
if [ "${PLATFORM}" = "linux" ]; then
    install_linux
else
    install_macos
fi
