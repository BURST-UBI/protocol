#!/usr/bin/env bash
set -euo pipefail
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
source /lab/common.sh
for name in public client1 client2; do
  if test -f "/evidence/$name.pid"; then
    read -r pid < "/evidence/$name.pid"
    [[ "$pid" =~ ^[0-9]+$ ]]
    kill -KILL "$pid" 2>/dev/null || true
  fi
done
ip netns exec router iptables -t nat -A PREROUTING -i outside -p tcp --dport 17076 -j DNAT --to-destination 10.99.0.2:7076
ip netns exec router iptables -A FORWARD -i outside -o lan -p tcp -d 10.99.0.2 --dport 7076 -j ACCEPT
ip netns exec client1 /bin/burst-daemon --config /lab/manual-client.toml node run > /evidence/manual-client.log 2>&1 &
echo $! > /evidence/manual-client.pid
# Only the public node initiates: fresh data, target is the forwarded WAN port.
/bin/burst-daemon --network dev --data-dir /evidence/manual-public --port 7076 --rpc-port 7077 --rpc-bind 172.30.99.2 --disable-upnp --bootstrap-peers 172.30.99.1:17076 node run > /evidence/manual-public.log 2>&1 &
echo $! > /evidence/manual-public.pid
wait_for_node_rpcs dev 90 http://172.30.99.2:7077 http://10.99.0.2:7077
ready=false
for attempt in $(seq 1 90); do
  if curl -fsS --max-time 2 http://172.30.99.2:7077 -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}' 2>/dev/null | jq -e '.result.count > 0' >/dev/null; then ready=true; break; fi
  sleep 1
done
test "$ready" = true
echo 'PASS public node connects through manually forwarded WAN port'
BURST_VALIDATION_REPORT=/evidence/manual-state.json /bin/validate_transport http://172.30.99.2:7077 http://10.99.0.2:7077
echo 'PASS signed wallet journey through inbound manual port forwarding'
