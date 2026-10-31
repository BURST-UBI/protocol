#!/usr/bin/env bash
# Requires the running IGD emulator, lab namespaces and manual-forward setup.
# First checks mapper state and emulator rules under renewals, conflicts and
# crashes. The final case separately exercises inbound P2P and signed payments
# through an alternate mapped port, then rejects a new connection after cleanup.
set -euo pipefail
source /lab/common.sh
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
control=http://10.99.0.1:1901/test
wait_log() {
  local pattern=$1 path=$2
  for attempt in $(seq 1 100); do
    if grep -q "$pattern" "$path"; then return 0; fi
    sleep 0.2
  done
  echo "Missing $pattern in $path"; return 1
}
wait_mappings() {
  for attempt in $(seq 1 100); do
    if curl -fsS --max-time 2 "$control/state" | jq -e "length == $1" >/dev/null; then return 0; fi
    sleep 0.2
  done
  return 1
}
curl -fsS "$control/public" >/dev/null
ip netns exec client1 env BURST_PROBE_LEASE_SECS=4 BURST_PROBE_HOLD_SECS=35 /bin/upnp_probe > /evidence/upnp-lifecycle.log 2>&1 &
probe=$!
wait_log Active /evidence/upnp-lifecycle.log
curl -fsS "$control/reject" >/dev/null
wait_log Failed /evidence/upnp-lifecycle.log
wait_mappings 0
curl -fsS "$control/public" >/dev/null
wait_mappings 1
curl -fsS "$control/cgnat" >/dev/null
wait_log NonRoutable /evidence/upnp-lifecycle.log
wait_mappings 0
curl -fsS "$control/public" >/dev/null
wait_mappings 1
wait "$probe"
wait_mappings 0
test "$(grep -c 'UPnP state: Active' /evidence/upnp-lifecycle.log)" -ge 4
echo 'PASS lease renewal, failed renewal withdrawal, rediscovery, WAN CGNAT change and recovery'

# Actual competing clients: same local port, distinct external mappings.
for n in 1 2; do
  ip netns exec "client$n" env BURST_PROBE_LEASE_SECS=4 BURST_PROBE_HOLD_SECS=12 /bin/upnp_probe > "/evidence/upnp-shared$n.log" 2>&1 &
  if test "$n" = 1; then first=$!; else second=$!; fi
done
wait_mappings 2
curl -fsS "$control/state" | jq -e '([.[].port] | unique | length) == 2 and ([.[].client] | unique | length) == 2' >/dev/null
wait "$first"; wait "$second"
wait_mappings 0
echo 'PASS two LAN nodes map the same local port without replacing each other'

# Verify the emulator expires the finite lease after an abrupt probe death.
ip netns exec client1 env BURST_PROBE_LEASE_SECS=4 BURST_PROBE_HOLD_SECS=30 /bin/upnp_probe > /evidence/upnp-crash.log 2>&1 &
probe=$!
wait_mappings 1
kill -KILL "$probe"
wait "$probe" 2>/dev/null || true
wait_mappings 0
grep -q 'LeaseExpired' /evidence/igd.log
echo 'PASS finite lease expires after mapper crash'

# A router may lose our lease on reboot and give that port to another client.
# Never delete that client's mapping when renewal reports a conflict.
ip netns exec client1 env BURST_PROBE_LEASE_SECS=4 BURST_PROBE_HOLD_SECS=15 /bin/upnp_probe > /evidence/upnp-reassigned.log 2>&1 &
probe=$!
wait_mappings 1
curl -fsS "$control/steal" >/dev/null
wait_log Failed /evidence/upnp-reassigned.log
wait_mappings 2
curl -fsS "$control/state" | jq -e 'any(.[]; .port == 7076 and .client == "10.99.0.3")' >/dev/null
wait "$probe"
wait_mappings 1
curl -fsS "$control/state" | jq -e '.[0].port == 7076 and .[0].client == "10.99.0.3"' >/dev/null
curl -fsS "$control/release-other" >/dev/null
echo 'PASS renewal conflict preserves another client’s reassigned mapping'

# Forward a REAL inbound P2P connection through the mapping, using a nondefault
# external port. Existing manual rules are removed so they cannot mask failure.
for name in manual-public manual-client; do
  read -r pid < "/evidence/$name.pid"; [[ "$pid" =~ ^[0-9]+$ ]]; kill -KILL "$pid" 2>/dev/null || true
done
ip netns exec router iptables -t nat -D PREROUTING -i outside -p tcp --dport 17076 -j DNAT --to-destination 10.99.0.2:7076
ip netns exec router iptables -D FORWARD -i outside -o lan -p tcp -d 10.99.0.2 --dport 7076 -j ACCEPT
ip -n router addr add 8.8.4.4/32 dev outside
ip route add 8.8.4.4/32 via 172.30.99.1
ip netns exec client1 /bin/burst-daemon --config /lab/upnp-client.toml node run > /evidence/upnp-client.log 2>&1 &
echo $! > /evidence/upnp-client.pid
curl -fsS "$control/conflict" >/dev/null
ip netns exec client1 env BURST_PROBE_LEASE_SECS=4 BURST_PROBE_HOLD_SECS=75 /bin/upnp_probe > /evidence/upnp-forward.log 2>&1 &
probe=$!
wait_mappings 1
port=$(curl -fsS "$control/state" | jq -er '.[0].port')
test "$port" != 7076
/bin/burst-daemon --network dev --data-dir /evidence/upnp-public --port 7076 --rpc-port 7077 --rpc-bind 172.30.99.2 --disable-upnp --bootstrap-peers "8.8.4.4:$port" node run > /evidence/upnp-public.log 2>&1 &
echo $! > /evidence/upnp-public.pid
wait_for_node_rpcs dev 45 http://172.30.99.2:7077 http://10.99.0.2:7077
ready=false
for attempt in $(seq 1 45); do
  if curl -fsS --max-time 2 http://172.30.99.2:7077 -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}' 2>/dev/null | jq -e '.result.count == 1' >/dev/null; then ready=true; break; fi
  sleep 1
done
test "$ready" = true
BURST_VALIDATION_REPORT=/evidence/upnp-state.json /bin/validate_transport http://172.30.99.2:7077 http://10.99.0.2:7077
wait "$probe"
wait_mappings 0
if timeout 2 bash -c "exec 3<>/dev/tcp/8.8.4.4/$port"; then echo 'FAIL mapping still accepts new inbound connections'; exit 1; fi
echo 'PASS inbound signed wallet journey through renewed UPnP mapping, alternate port and shutdown cleanup'
curl -fsS "$control/public" >/dev/null
