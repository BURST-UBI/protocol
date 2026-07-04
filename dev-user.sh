#!/usr/bin/env bash
set -euo pipefail

# dev-user.sh — Run ONE local node on the default port (7077) so the
# desktop wallet and node-gui connect to it with zero configuration, giving
# you an as-close-to-real single-user BURST environment.
#
# For multi-node network realism (propagation, sync, consensus) use
# dev-cluster.sh instead. This script is for driving the *user* journey
# through the real GUIs.
#
# Usage:
#   ./dev-user.sh                 # build + run one faucet node at :7077
#   ./dev-user.sh --no-build      # reuse existing binary
#   ./dev-user.sh --keep          # keep the data dir on exit
#   ./dev-user.sh --debug         # verbose logs

BINARY="./target/release/burst-daemon"
DATA_DIR=".dev_user_node"
RPC_PORT=7077          # matches wallet + node-gui defaults exactly
P2P_PORT=27076
WS_PORT=7078
LOG_LEVEL="info"
SKIP_BUILD=false
KEEP_DATA=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) SKIP_BUILD=true; shift ;;
        --keep)     KEEP_DATA=true; shift ;;
        --debug)    LOG_LEVEL="debug"; shift ;;
        --help|-h)  sed -n '3,15p' "$0"; exit 0 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

CYAN='\033[0;36m'; GREEN='\033[0;32m'; BOLD='\033[1m'; NC='\033[0m'
info() { echo -e "${CYAN}[dev-user]${NC} $*"; }
ok()   { echo -e "${GREEN}[   ok   ]${NC} $*"; }

cleanup() {
    echo
    info "Shutting down node"
    [[ -n "${NODE_PID:-}" ]] && kill "$NODE_PID" 2>/dev/null || true
    [[ -n "${NODE_PID:-}" ]] && wait "$NODE_PID" 2>/dev/null || true
    if [ "$KEEP_DATA" = false ]; then rm -rf "$DATA_DIR"; info "Removed $DATA_DIR"; fi
}
trap cleanup EXIT INT TERM

if [ "$SKIP_BUILD" = false ]; then
    info "Building burst-daemon (release)…"
    cargo build --release --bin burst-daemon 2>&1 | tail -1
fi
[ -f "$BINARY" ] || { echo "Binary missing at $BINARY"; exit 1; }

rm -rf "$DATA_DIR"; mkdir -p "$DATA_DIR"

info "Starting node — RPC :$RPC_PORT  WS :$WS_PORT  (faucet ON)"
$BINARY \
    --network dev \
    --data-dir "$DATA_DIR" \
    --port "$P2P_PORT" \
    --rpc-port "$RPC_PORT" \
    --websocket --websocket-port "$WS_PORT" \
    --faucet \
    --disable-upnp \
    --log-level "$LOG_LEVEL" \
    node run > "$DATA_DIR/node.log" 2>&1 &
NODE_PID=$!

rpc() { curl -s --max-time 5 -X POST "http://127.0.0.1:$RPC_PORT" \
    -H 'Content-Type: application/json' -d "$1" 2>/dev/null; }

for _ in $(seq 1 40); do
    rpc '{"action":"node_info"}' | jq -e '.result' >/dev/null 2>&1 && break
    sleep 0.5
done
ok "Node online at http://127.0.0.1:$RPC_PORT"

echo
echo -e "${BOLD}Point the apps here (both default to :7077 already):${NC}"
echo "  Wallet (desktop):   cd wallet    && npm install && npm run tauri dev"
echo "  Node GUI (browser): cd node-gui  && npm install && npm run dev"
echo
echo -e "${BOLD}Bootstrap a fresh wallet from the CLI (the faucet shortcut):${NC}"
echo "  W=\$(curl -s localhost:$RPC_PORT -d '{\"action\":\"wallet_create_full\"}')"
echo "  A=\$(echo \"\$W\" | jq -r .result.address); K=\$(echo \"\$W\" | jq -r .result.private_key)"
echo "  curl -s localhost:$RPC_PORT -d \"{\\\"action\\\":\\\"faucet\\\",\\\"account\\\":\\\"\$A\\\"}\" | jq"
echo "  curl -s localhost:$RPC_PORT -d \"{\\\"action\\\":\\\"account_info\\\",\\\"account\\\":\\\"\$A\\\"}\" | jq"
echo
echo "  Logs: tail -f $DATA_DIR/node.log      Stop: Ctrl+C"
echo
wait "$NODE_PID"
