#!/usr/bin/env bash
# Requires the live lab after reconnect.sh and its saved success marker.
# Starts client2 behind the same router, logs peer state and runs the transport
# journey against all three RPC endpoints. The marker alone is not new evidence.
set -euo pipefail
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
test -f /evidence/reconnect-passed
# Both clients use 7076 behind the same translated public IP.
ip netns exec client2 /bin/burst-daemon --network dev --no-representative --data-dir /evidence/client2 --port 7076 --rpc-port 7077 --rpc-bind 0.0.0.0 --disable-upnp --bootstrap-peers 172.30.99.2:7076 node run > /evidence/client2.log 2>&1 &
echo $! > /evidence/client2.pid
sleep 15
for attempt in $(seq 1 6); do
  for addr in 172.30.99.2 10.99.0.2 10.99.0.3; do
    printf '%s ' "$addr"
    curl -fsS --max-time 3 "http://$addr:7077" -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}'
    printf '\n'
  done
  sleep 5
done
BURST_VALIDATION_REPORT=/evidence/multi-client-state.json /bin/validate_transport http://172.30.99.2:7077 http://10.99.0.2:7077 http://10.99.0.3:7077
echo 'PASS two default-port nodes share one NAT address without losing either peer'
