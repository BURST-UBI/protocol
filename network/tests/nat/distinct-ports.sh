#!/usr/bin/env bash
# Contrast with multiple-clients.sh: only client 2's listening port changes.
set -euo pipefail
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
source /lab/common.sh
read -r pid < /evidence/client2.pid
[[ "$pid" =~ ^[0-9]+$ ]]
kill -KILL "$pid" 2>/dev/null || true
ip netns exec client2 /bin/burst-daemon --network dev --no-representative --data-dir /evidence/client2 --port 7086 --rpc-port 7077 --rpc-bind 0.0.0.0 --disable-upnp --bootstrap-peers 172.30.99.2:7076 node run > /evidence/client2-distinct.log 2>&1 &
echo $! > /evidence/client2.pid
wait_for_node_rpcs dev 90 http://172.30.99.2:7077 http://10.99.0.2:7077 http://10.99.0.3:7077
ready=false
for attempt in $(seq 1 90); do
  if curl -fsS --max-time 2 http://172.30.99.2:7077 -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}' | jq -e '.result.count == 2' >/dev/null; then ready=true; break; fi
  sleep 1
done
test "$ready" = true
echo 'PASS public node retains both same-NAT clients when their listening ports differ'
BURST_VALIDATION_REPORT=/evidence/distinct-port-state.json /bin/validate_transport http://172.30.99.2:7077 http://10.99.0.2:7077 http://10.99.0.3:7077
echo 'PASS signed wallet journey with distinct-port clients behind one NAT'
