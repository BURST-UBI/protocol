#!/usr/bin/env bash
# Run ONLY inside the isolated, network=none NAT-lab container.
set -euo pipefail
test -f /.dockerenv
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
mkdir -p /evidence
ip netns add router
ip netns add client1
ip netns add client2
ip link add wan type veth peer name outside
ip link set outside netns router
ip addr add 172.30.99.2/24 dev wan
ip link set wan up
ip -n router addr add 172.30.99.1/24 dev outside
ip -n router link set outside up
ip -n router link set lo up
ip -n router link add lan type bridge
ip -n router addr add 10.99.0.1/24 dev lan
ip -n router link set lan up
for n in 1 2; do
  ip link add "lan$n" type veth peer name eth0 netns "client$n"
  ip link set "lan$n" netns router
  ip -n router link set "lan$n" master lan
  ip -n router link set "lan$n" up
  ip -n "client$n" link set lo up
  ip -n "client$n" addr add "10.99.0.$((n+1))/24" dev eth0
  ip -n "client$n" link set eth0 up
  ip -n "client$n" route add default via 10.99.0.1
done
ip route add 10.99.0.0/24 via 172.30.99.1
ip netns exec router sysctl -qw net.ipv4.ip_forward=1
ip netns exec router iptables -t nat -A POSTROUTING -s 10.99.0.0/24 -o outside -j MASQUERADE
ip netns exec router iptables -P FORWARD DROP
ip netns exec router iptables -A FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
ip netns exec router iptables -A FORWARD -i lan -o outside -j ACCEPT
# Test-controller access to RPC only; no inbound P2P exception.
ip netns exec router iptables -A FORWARD -i outside -o lan -p tcp --dport 7077 -j ACCEPT
/bin/burst-daemon --network dev --data-dir /evidence/public --port 7076 --rpc-port 7077 --rpc-bind 172.30.99.2 --disable-upnp node run > /evidence/public.log 2>&1 &
echo $! > /evidence/public.pid
ip netns exec client1 /bin/burst-daemon --network dev --no-representative --data-dir /evidence/client1 --port 7076 --rpc-port 7077 --rpc-bind 0.0.0.0 --disable-upnp --bootstrap-peers 172.30.99.2:7076 node run > /evidence/client1.log 2>&1 &
echo $! > /evidence/client1.pid
rpc() { curl -fsS --max-time 3 "http://$1:7077" -H 'Content-Type: application/json' -d "{\"action\":\"$2\"}"; }
for addr in 172.30.99.2 10.99.0.2; do
  ready=false
  for attempt in $(seq 1 60); do
    if rpc "$addr" telemetry 2>/dev/null | jq -e '.result' >/dev/null; then ready=true; break; fi
    sleep 0.5
  done
  test "$ready" = true
done
if timeout 2 bash -c 'exec 3<>/dev/tcp/10.99.0.2/7076'; then
  echo 'FAIL: unsolicited inbound P2P unexpectedly reachable'; exit 1
fi
echo 'PASS router blocks unsolicited inbound P2P'
sleep 3
rpc 172.30.99.2 peers_detailed | tee /evidence/one-client-peers.json
BURST_VALIDATION_REPORT=/evidence/one-client-state.json /bin/validate_transport http://172.30.99.2:7077 http://10.99.0.2:7077
echo 'PASS signed wallet journey through outbound-only source NAT'
touch /evidence/one-client-passed
# Keep namespaces available for separate reconnect/multiple-client checks.
sleep infinity &
wait
