#!/usr/bin/env bash
# Requires the running emulator and successful lifecycle forwarding setup.
# Uses Test-network daemons to check alternate-port advertisement, discovery
# and graceful mapping removal. Seed/discoverer processes and SNAT remain.
set -euo pipefail
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
source /lab/common.sh
for name in upnp-public upnp-client; do
  read -r pid < "/evidence/$name.pid"; [[ "$pid" =~ ^[0-9]+$ ]]; kill -KILL "$pid" 2>/dev/null || true
done
# The emulator's WAN IP must also be the observed outbound source IP.
ip netns exec router iptables -t nat -I POSTROUTING 1 -s 10.99.0.0/24 -o outside -j SNAT --to-source 8.8.4.4
curl -fsS http://10.99.0.1:1901/test/public >/dev/null
curl -fsS http://10.99.0.1:1901/test/conflict >/dev/null
/bin/burst-daemon --network test --data-dir /evidence/upnp-native-public --port 7076 --rpc-port 7077 --rpc-bind 172.30.99.2 --disable-upnp node run > /evidence/upnp-native-public.log 2>&1 &
ip netns exec client1 /bin/burst-daemon --config /lab/upnp-native-client.toml node run > /evidence/upnp-native-client.log 2>&1 &
client=$!
mapped=false
for attempt in $(seq 1 60); do
  if curl -fsS http://10.99.0.1:1901/test/state | jq -e 'length == 1 and .[0].port != 7076' >/dev/null; then mapped=true; break; fi
  sleep 0.5
done
test "$mapped" = true
# This node knows only the seed. It must learn the private client's alternate
# external port through the daemon's UPnP and keepalive gossip path.
/bin/burst-daemon --network test --data-dir /evidence/upnp-native-discoverer --port 7086 --rpc-port 7087 --rpc-bind 172.30.99.2 --disable-upnp --bootstrap-peers 172.30.99.2:7076 node run > /evidence/upnp-native-discoverer.log 2>&1 &
wait_for_node_rpcs test 90 http://172.30.99.2:7077 http://10.99.0.2:7077 http://172.30.99.2:7087
discovered=false
for attempt in $(seq 1 160); do
  if curl -fsS --max-time 2 http://172.30.99.2:7087 -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}' 2>/dev/null | jq -e '.result.count == 2' >/dev/null; then discovered=true; break; fi
  sleep 1
done
test "$discovered" = true
curl -fsS --max-time 2 http://10.99.0.2:7077 -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}' | tee /evidence/upnp-native-peers.json | jq -e '.result.count == 2' >/dev/null
echo 'PASS real daemon maps an alternate external port and publishes it for automatic peer discovery'
kill -TERM "$client"
wait "$client"
curl -fsS http://10.99.0.1:1901/test/state | jq -e 'length == 0' >/dev/null
echo 'PASS real daemon shutdown removes its UPnP mapping'
curl -fsS http://10.99.0.1:1901/test/public >/dev/null
