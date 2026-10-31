#!/usr/bin/env bash
set -euo pipefail

# Run a local Dev faucet node on RPC port 7077 for the desktop apps.
# The default app connection points to that port.
# This is a development fixture, not mainnet or release qualification.
#
# dev-cluster.sh runs several local nodes for propagation and sync checks.
# This script keeps one ledger for repeated GUI development.
# Builds use the release profile; follow the host validation policy.
#
# Usage:
#   ./dev-user.sh                 # build + run one faucet node at :7077
#   ./dev-user.sh --no-build      # reuse existing binary
#   ./dev-user.sh --keep          # compatibility flag; data is always kept
#   ./dev-user.sh --debug         # verbose logs
#   ./dev-user.sh --data-dir PATH # use a separate ledger; never deletes old data

BINARY="./target/release/burst-daemon"
DATA_DIR=".dev_user_node"
RPC_PORT=7077          # matches wallet + node-gui defaults exactly
P2P_PORT=27076
WS_PORT=7078
LOG_LEVEL="info"
SKIP_BUILD=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) SKIP_BUILD=true; shift ;;
        --keep)     shift ;;
        --debug)    LOG_LEVEL="debug"; shift ;;
        --data-dir)
            if [[ $# -lt 2 || -z "$2" || "$2" == --* ]]; then
                echo "--data-dir requires a directory path" >&2
                exit 1
            fi
            DATA_DIR="$2"; shift 2 ;;
        --help|-h)  sed -n '3,17p' "$0"; exit 0 ;;
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
    info "Data preserved at $DATA_DIR (reused on the next run)"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ "$SKIP_BUILD" = false ]; then
    info "Building burst-daemon (release)…"
    cargo build --release --bin burst-daemon 2>&1 | tail -1
fi
[ -f "$BINARY" ] || { echo "Binary missing at $BINARY"; exit 1; }

mkdir -p "$DATA_DIR"

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
    node run >> "$DATA_DIR/node.log" 2>&1 &
NODE_PID=$!

rpc() { curl -s --max-time 5 -X POST "http://127.0.0.1:$RPC_PORT" \
    -H 'Content-Type: application/json' -d "$1" 2>/dev/null; }

READY=false
for _ in $(seq 1 40); do
    kill -0 "$NODE_PID" 2>/dev/null || break
    if rpc '{"action":"node_info"}' | jq -e '.result' >/dev/null 2>&1; then
        READY=true
        break
    fi
    sleep 0.5
done
[ "$READY" = true ] && kill -0 "$NODE_PID" 2>/dev/null || {
    echo "Node failed to start; inspect $DATA_DIR/node.log"
    exit 1
}
ok "Node online at http://127.0.0.1:$RPC_PORT"

echo
echo -e "${BOLD}Point the apps here (both default to :7077 already):${NC}"
echo "  Wallet (desktop):   cd ../wallet   && npm run tauri dev"
echo "  Node GUI (desktop): cd ../node-gui && npm run tauri dev"
echo
echo -e "${BOLD}Bootstrap a fresh wallet from the CLI (the faucet shortcut):${NC}"
echo "  W=\$(curl -s localhost:$RPC_PORT -d '{\"action\":\"wallet_create_full\"}')"
echo "  A=\$(echo \"\$W\" | jq -r .result.address); K=\$(echo \"\$W\" | jq -r .result.private_key)"
echo "  curl -s localhost:$RPC_PORT -d \"{\\\"action\\\":\\\"faucet\\\",\\\"account\\\":\\\"\$A\\\"}\" | jq"
echo "  curl -s localhost:$RPC_PORT -d \"{\\\"action\\\":\\\"account_info\\\",\\\"account\\\":\\\"\$A\\\"}\" | jq"
echo
echo "  Faucet returns pending_receive and send_block_hash, not a spendable balance."
echo "  Accept that hash with the wallet or receive_simple before spending."
echo "  For the desktop wallet, faucet its public address; keep its private key in the wallet."
echo "  Test funds have no real value. New test identities use the genesis bootstrap window."
echo "  Logs: tail -f $DATA_DIR/node.log      Stop: Ctrl+C"
echo
wait "$NODE_PID"
