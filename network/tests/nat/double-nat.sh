#!/usr/bin/env bash
# Requires the running single-router lab. Replaces its processes with separate
# double-NAT data directories, adds an ISP layer and blocks unsolicited P2P.
# Controller RPC remains explicitly reachable. Topology is restored only after
# success; a failure leaves the container for diagnosis.
set -euo pipefail
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
source /lab/common.sh
for name in public client1 client2; do
  read -r pid < "/evidence/$name.pid"
  [[ "$pid" =~ ^[0-9]+$ ]]
  kill -KILL "$pid" 2>/dev/null || true
done
ip netns add isp
ip link set wan netns isp
# Moving an interface to another namespace clears its IP configuration.
ip -n isp addr add 100.64.0.1/24 dev wan
ip -n isp link set wan up
ip -n isp link set lo up
ip -n router addr del 172.30.99.1/24 dev outside
ip -n router addr add 100.64.0.2/24 dev outside
ip -n router route add default via 100.64.0.1
ip link add wan type veth peer name uplink netns isp
ip addr add 172.30.99.2/24 dev wan
ip link set wan up
ip -n isp addr add 172.30.99.1/24 dev uplink
ip -n isp link set uplink up
ip -n isp route add 10.99.0.0/24 via 100.64.0.2
ip route replace 10.99.0.0/24 via 172.30.99.1
ip netns exec isp sysctl -qw net.ipv4.ip_forward=1
ip netns exec isp iptables -t nat -A POSTROUTING -s 100.64.0.0/24 -o uplink -j MASQUERADE
ip netns exec isp iptables -P FORWARD DROP
ip netns exec isp iptables -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
ip netns exec isp iptables -A FORWARD -i wan -o uplink -j ACCEPT
ip netns exec isp iptables -A FORWARD -i uplink -o wan -p tcp --dport 7077 -j ACCEPT
/bin/burst-daemon --network dev --data-dir /evidence/double-public --port 7076 --rpc-port 7077 --rpc-bind 172.30.99.2 --disable-upnp node run > /evidence/double-public.log 2>&1 &
echo $! > /evidence/public.pid
for n in 1 2; do
  ip netns exec "client$n" /bin/burst-daemon --network dev --no-representative --data-dir "/evidence/double-client$n" --port 7076 --rpc-port 7077 --rpc-bind 0.0.0.0 --disable-upnp --bootstrap-peers 172.30.99.2:7076 node run > "/evidence/double-client$n.log" 2>&1 &
  echo $! > "/evidence/client$n.pid"
done
wait_for_node_rpcs dev 90 http://172.30.99.2:7077 http://10.99.0.2:7077 http://10.99.0.3:7077
ready=false
for attempt in $(seq 1 90); do
  if curl -fsS --max-time 2 http://172.30.99.2:7077 -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}' 2>/dev/null | jq -e '.result.count == 2' >/dev/null; then ready=true; break; fi
  sleep 1
done
test "$ready" = true
if timeout 2 bash -c 'exec 3<>/dev/tcp/10.99.0.2/7076'; then echo 'FAIL CGNAT allowed unsolicited inbound'; exit 1; fi
BURST_VALIDATION_REPORT=/evidence/double-nat-state.json /bin/validate_transport http://172.30.99.2:7077 http://10.99.0.2:7077 http://10.99.0.3:7077
echo 'PASS two default-port nodes through home NAT plus ISP CGNAT, inbound blocked'
# Restore single-router topology for subsequent manual/UPnP scenarios.
for name in public client1 client2; do
  read -r pid < "/evidence/$name.pid"; kill -KILL "$pid" 2>/dev/null || true
done
ip link del wan
ip -n isp link set wan netns 1
ip netns del isp
ip addr add 172.30.99.2/24 dev wan
ip link set wan up
ip -n router addr del 100.64.0.2/24 dev outside
ip -n router addr add 172.30.99.1/24 dev outside
ip route replace 10.99.0.0/24 via 172.30.99.1
