#!/usr/bin/env bash
set -euo pipefail

# dev-cluster.sh — Spin up a local 3-node BURST dev cluster for E2E testing.
#
# Usage:
#   ./dev-cluster.sh              # build + run cluster + smoke test
#   ./dev-cluster.sh --no-build   # skip build, use existing binary
#   ./dev-cluster.sh --nodes 5    # run 5 nodes instead of 3
#   ./dev-cluster.sh --keep       # compatibility flag; logs/data always kept
#   ./dev-cluster.sh --smoke-only # exit after checks (nonzero on failure)
#
# Requires: cargo, curl, jq

# ── Configuration ─────────────────────────────────────────────────────

NUM_NODES=3
SKIP_BUILD=false
BINARY="./target/release/burst-daemon"
BASE_P2P_PORT=27076
BASE_RPC_PORT=17077
BASE_WS_PORT=17078
DATA_PREFIX=""
SMOKE_ONLY=false
LOG_LEVEL="info"
STARTUP_WAIT=4
SYNC_WAIT=3

# ── Parse arguments ──────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build)  SKIP_BUILD=true; shift ;;
        --nodes)     NUM_NODES="$2"; shift 2 ;;
        --keep)      shift ;;
        --debug)     LOG_LEVEL="debug"; shift ;;
        --smoke-only) SMOKE_ONLY=true; shift ;;
        --help|-h)
            sed -n '3,10p' "$0"
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

[[ "$NUM_NODES" =~ ^[1-9][0-9]*$ ]] && [ "$NUM_NODES" -le 20 ] || {
    echo "--nodes must be between 1 and 20"; exit 1;
}
# A fresh run never overwrites a previous test ledger. Keep logs for diagnosis.
RUN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/burst-dev-cluster.XXXXXXXX")
DATA_PREFIX="$RUN_DIR/node"

# ── Colors ───────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}[cluster]${NC} $*"; }
ok()    { echo -e "${GREEN}[  ok  ]${NC} $*"; }
fail()  { echo -e "${RED}[ fail ]${NC} $*"; }
warn()  { echo -e "${YELLOW}[ warn ]${NC} $*"; }
header(){ echo -e "\n${BOLD}═══ $* ═══${NC}"; }

# ── Cleanup on exit ──────────────────────────────────────────────────

PIDS=()

cleanup() {
    header "Shutting down cluster"
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
        fi
    done
    # Wait for processes to exit
    for pid in "${PIDS[@]}"; do
        wait "$pid" 2>/dev/null || true
    done
    info "Test data and logs preserved: $RUN_DIR"
    info "Done"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# ── Helpers ──────────────────────────────────────────────────────────

rpc() {
    local port="$1"
    local payload="$2"
    curl -s --max-time 35 -X POST "http://127.0.0.1:${port}" \
        -H "Content-Type: application/json" \
        -d "$payload" 2>/dev/null
}

rpc_result() {
    local port="$1"
    local payload="$2"
    rpc "$port" "$payload" | jq -r '.result // .error // empty'
}

wait_for_rpc() {
    local port="$1"
    local name="$2"
    local max_attempts=20
    local attempt
    for attempt in $(seq 1 $max_attempts); do
        if rpc "$port" '{"action":"node_info"}' | jq -e '.result' >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

# ── Step 1: Build ────────────────────────────────────────────────────

header "Build"

if [ "$SKIP_BUILD" = true ]; then
    if [ ! -f "$BINARY" ]; then
        fail "Binary not found at $BINARY — run without --no-build first"
        exit 1
    fi
    info "Skipping build (using existing binary)"
else
    info "Building burst-daemon (release)..."
    cargo build --release --bin burst-daemon 2>&1 | tail -1
    ok "Build complete"
fi

# ── Step 2: Start nodes ─────────────────────────────────────────────

header "Starting ${NUM_NODES}-node dev cluster"

for i in $(seq 1 "$NUM_NODES"); do
    p2p_port=$((BASE_P2P_PORT + (i - 1) * 10))
    rpc_port=$((BASE_RPC_PORT + (i - 1) * 10))
    ws_port=$((BASE_WS_PORT + (i - 1) * 10))
    data_dir="${DATA_PREFIX}_node${i}"
    log_file="${data_dir}/node.log"

    mkdir -p "$data_dir"

    if [ "$i" -eq 1 ]; then
        # Seed node: no bootstrap peers, faucet enabled
        $BINARY \
            --network dev \
            --data-dir "$data_dir" \
            --port "$p2p_port" \
            --rpc-port "$rpc_port" \
            --websocket --websocket-port "$ws_port" \
            --faucet \
            --disable-upnp \
            --log-level "$LOG_LEVEL" \
            node run > "$log_file" 2>&1 &
    else
        # Non-seed: bootstrap from node 1
        $BINARY \
            --network dev \
            --data-dir "$data_dir" \
            --port "$p2p_port" \
            --rpc-port "$rpc_port" \
            --websocket --websocket-port "$ws_port" \
            --bootstrap-peers "127.0.0.1:${BASE_P2P_PORT}" \
            --disable-upnp \
            --log-level "$LOG_LEVEL" \
            node run > "$log_file" 2>&1 &
    fi

    PIDS+=($!)
    info "Node $i: P2P=$p2p_port  RPC=$rpc_port  WS=$ws_port  PID=${PIDS[-1]}"
done

# ── Step 3: Wait for nodes to come online ────────────────────────────

header "Waiting for nodes"

ALL_UP=true
for i in $(seq 1 "$NUM_NODES"); do
    rpc_port=$((BASE_RPC_PORT + (i - 1) * 10))
    if wait_for_rpc "$rpc_port" "node$i"; then
        ok "Node $i is online (RPC :$rpc_port)"
    else
        fail "Node $i failed to start — check ${DATA_PREFIX}_node${i}/node.log"
        ALL_UP=false
    fi
done

if [ "$ALL_UP" = false ]; then
    fail "Not all nodes started. Aborting."
    exit 1
fi

# Give nodes time to peer with each other
info "Waiting ${SYNC_WAIT}s for peering..."
sleep "$SYNC_WAIT"

# ── Step 4: Check peering ────────────────────────────────────────────

header "Peer connectivity"

PEERING_FAILURES=0

for i in $(seq 1 "$NUM_NODES"); do
    rpc_port=$((BASE_RPC_PORT + (i - 1) * 10))
    peer_count=0
    for attempt in $(seq 1 30); do
        peer_count=$(rpc "$rpc_port" '{"action":"peers"}' | jq -r '.result.count // 0')
        [ "$peer_count" -gt 0 ] && break
        [ "$NUM_NODES" -eq 1 ] && break
        sleep 1
    done
    if [ "$peer_count" -gt 0 ]; then
        ok "Node $i: $peer_count peer(s) connected"
    elif [ "$NUM_NODES" -gt 1 ]; then
        fail "Node $i: no peer connection within 30 seconds"
        PEERING_FAILURES=$((PEERING_FAILURES + 1))
    fi
done

# ── Step 5: Smoke tests ─────────────────────────────────────────────

header "Smoke tests"

SEED_RPC="$BASE_RPC_PORT"
NODE2_RPC=$((BASE_RPC_PORT + 10))
TESTS_PASSED=0
TESTS_FAILED=$PEERING_FAILURES
TESTS_SKIPPED=0

pass() { TESTS_PASSED=$((TESTS_PASSED + 1)); ok "$1"; }
test_fail() { TESTS_FAILED=$((TESTS_FAILED + 1)); fail "$1"; }

# Test 1: Create a wallet
info "Test 1: Create wallet via wallet_create_full"
WALLET=$(rpc "$SEED_RPC" '{"action":"wallet_create_full"}')
ADDRESS=$(echo "$WALLET" | jq -r '.result.address // empty')
PRIVKEY=$(echo "$WALLET" | jq -r '.result.private_key // empty')

if [ -n "$ADDRESS" ] && [ -n "$PRIVKEY" ]; then
    pass "Wallet created: ${ADDRESS:0:15}..."
else
    test_fail "wallet_create_full did not return an address and key"
fi

# Test 2: Faucet — bootstrap verification and a real pending burn payment
info "Test 2: Faucet (verify + pending payment)"
FAUCET=$(rpc "$SEED_RPC" "{\"action\":\"faucet\",\"account\":\"$ADDRESS\"}")
FAUCET_STATUS=$(echo "$FAUCET" | jq -r '.result.status // empty')

FUND_HASH=$(echo "$FAUCET" | jq -r '.result.send_block_hash // empty')
if [ "$FAUCET_STATUS" = "pending_receive" ] && [ -n "$FUND_HASH" ]; then
    pass "Faucet payment confirmed and pending recipient acceptance"
    FUND_RECV=$(rpc "$SEED_RPC" "{\"action\":\"receive_simple\",\"private_key\":\"$PRIVKEY\",\"send_block_hash\":\"$FUND_HASH\"}")
    if echo "$FUND_RECV" | jq -e '.result.block_hash' >/dev/null; then
        pass "Recipient signed the initial receive-open"
    else
        test_fail "Initial receive failed: $FUND_RECV"
    fi
else
    test_fail "Faucet failed: $FAUCET"
fi

# Test 3: Check account_info on seed node
info "Test 3: account_info on seed node"
ACCT_INFO=$(rpc "$SEED_RPC" "{\"action\":\"account_info\",\"account\":\"$ADDRESS\"}")
TRST_BAL=$(echo "$ACCT_INFO" | jq -r '.result.trst_balance // "0"')
ACCT_STATE=$(echo "$ACCT_INFO" | jq -r '.result.verification_state // empty')

if [ "$ACCT_STATE" = "verified" ] && [ "$TRST_BAL" != "0" ]; then
    pass "Account verified, TRST balance: $TRST_BAL"
else
    test_fail "account_info unexpected: state=$ACCT_STATE balance=$TRST_BAL"
fi

# Test 4: Burn BRN → TRST
info "Test 4: burn_simple (BRN → TRST conversion)"
# Accrual uses whole seconds; wait for the fresh account to earn BRN.
sleep 1
BURN=$(rpc "$SEED_RPC" "{\"action\":\"burn_simple\",\"private_key\":\"$PRIVKEY\",\"amount\":\"100\"}")
BURN_HASH=$(echo "$BURN" | jq -r '.result.block_hash // empty')
BURN_ERR=$(echo "$BURN" | jq -r '.error // empty')

if [ -n "$BURN_HASH" ]; then
    pass "Burn block: ${BURN_HASH:0:16}..."
else
    test_fail "Burn failed: $BURN"
fi

# Test 5: Create a second wallet and send TRST
info "Test 5: send_simple (TRST transfer)"
WALLET2=$(rpc "$SEED_RPC" '{"action":"wallet_create_full"}')
ADDRESS2=$(echo "$WALLET2" | jq -r '.result.address // empty')
PRIVKEY2=$(echo "$WALLET2" | jq -r '.result.private_key // empty')

SEND=$(rpc "$SEED_RPC" "{\"action\":\"send_simple\",\"private_key\":\"$PRIVKEY\",\"destination\":\"$ADDRESS2\",\"amount\":\"1000000\",\"origin\":\"$FUND_HASH\"}")
SEND_HASH=$(echo "$SEND" | jq -r '.result.block_hash // empty')

if [ -n "$SEND_HASH" ]; then
    pass "Send block: ${SEND_HASH:0:16}..."
else
    SEND_ERR=$(echo "$SEND" | jq -r '.error // empty')
    test_fail "Send failed: $SEND_ERR"
fi

# Test 6: Receive the pending TRST
if [ -n "$SEND_HASH" ]; then
    info "Test 6: receive_simple (pocket pending)"

    # An unverified recipient may receive; receiving does not verify them.
    RECV=$(rpc "$SEED_RPC" "{\"action\":\"receive_simple\",\"private_key\":\"$PRIVKEY2\",\"send_block_hash\":\"$SEND_HASH\"}")
    RECV_HASH=$(echo "$RECV" | jq -r '.result.block_hash // empty')

    if [ -n "$RECV_HASH" ]; then
        pass "Receive block: ${RECV_HASH:0:16}..."
    else
        RECV_ERR=$(echo "$RECV" | jq -r '.error // empty')
        test_fail "Receive failed: $RECV_ERR"
    fi
else
    info "Test 6: skipped (no send block)"
fi

# Test 7: Verify account synced to node 2
if [ "$NUM_NODES" -ge 2 ]; then
    info "Test 7: Cross-node sync check"
    sleep 2  # allow time for block propagation

    ACCT2=$(rpc "$NODE2_RPC" "{\"action\":\"account_info\",\"account\":\"$ADDRESS\"}")
    STATE2=$(echo "$ACCT2" | jq -r '.result.verification_state // empty')

    if [ "$STATE2" = "verified" ]; then
        pass "Account synced to node 2"
    else
        test_fail "Verified account did not replicate to node 2"
    fi
fi

# Test 8: Telemetry / node_info
info "Test 8: node_info (telemetry)"
TELEM=$(rpc "$SEED_RPC" '{"action":"node_info"}')
BLOCKS=$(echo "$TELEM" | jq -r '.result.block_count // 0')
UPTIME=$(echo "$TELEM" | jq -r '.result.uptime_secs // 0')

if [ "$UPTIME" -gt 0 ]; then
    pass "Node info: blocks=$BLOCKS uptime=${UPTIME}s"
else
    test_fail "node_info failed: $TELEM"
fi

# Test 9: Governance proposal (if available)
info "Test 9: governance_proposals (list)"
GOV=$(rpc "$SEED_RPC" '{"action":"governance_proposals"}')
GOV_OK=$(echo "$GOV" | jq -r 'if .result != null then "ok" else empty end')

if [ -n "$GOV_OK" ]; then
    pass "Governance endpoint responsive"
else
    test_fail "governance_proposals failed: $GOV"
fi

# Test 10: Representatives
info "Test 10: representatives"
REPS=$(rpc "$SEED_RPC" '{"action":"representatives"}')
REPS_OK=$(echo "$REPS" | jq -r 'if .result != null then "ok" else empty end')

if [ -n "$REPS_OK" ]; then
    pass "Representatives endpoint responsive"
else
    test_fail "representatives failed: $REPS"
fi

# ── Results ──────────────────────────────────────────────────────────

header "Results"

echo ""
printf "  ${GREEN}Passed: %d${NC}\n" "$TESTS_PASSED"
printf "  ${RED}Failed: %d${NC}\n" "$TESTS_FAILED"
printf "  ${YELLOW}Skipped: %d${NC}\n" "$TESTS_SKIPPED"
echo ""

if [ "$TESTS_FAILED" -eq 0 ]; then
    ok "All executed smoke tests passed ($TESTS_SKIPPED skipped; not full E2E coverage)"
else
    fail "$TESTS_FAILED test(s) failed"
fi

if [ "$SMOKE_ONLY" = true ]; then
    [ "$TESTS_FAILED" -eq 0 ]
    exit $?
fi

# ── Interactive mode ─────────────────────────────────────────────────

header "Cluster is running"

echo ""
echo "  Seed node RPC:   http://127.0.0.1:${BASE_RPC_PORT}"
for i in $(seq 2 "$NUM_NODES"); do
    rpc_port=$((BASE_RPC_PORT + (i - 1) * 10))
    echo "  Node $i RPC:      http://127.0.0.1:${rpc_port}"
done
echo ""
echo "  Example commands:"
echo "    curl -s localhost:${BASE_RPC_PORT} -d '{\"action\":\"node_info\"}' | jq"
echo "    curl -s localhost:${BASE_RPC_PORT} -d '{\"action\":\"peers\"}' | jq"
echo "    curl -s localhost:${BASE_RPC_PORT} -d '{\"action\":\"wallet_create_full\"}' | jq"
echo ""
echo "  Press Ctrl+C to stop the cluster."
echo ""

# Wait for user to kill
wait
