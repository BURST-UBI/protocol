#!/usr/bin/env bash
set -euo pipefail
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
failures=0
if ip netns exec client1 /bin/upnp_probe > /evidence/upnp-no-gateway.log 2>&1; then
  echo 'FAIL mapping reported without a gateway'; failures=$((failures+1))
elif grep -q 'UPnP state: NotFound' /evidence/upnp-no-gateway.log; then
  echo 'PASS missing gateway is reported as NotFound'
else
  echo 'FAIL missing-gateway detection'; failures=$((failures+1))
fi
# Keep a mapper alive while the gateway is absent, then bring the router up.
# It must recover automatically, not require a node restart.
ip netns exec client1 env BURST_PROBE_LEASE_SECS=4 BURST_PROBE_HOLD_SECS=18 /bin/upnp_probe > /evidence/upnp-discovery-recovery.log 2>&1 &
recovery_pid=$!
for attempt in $(seq 1 70); do
  if grep -q NotFound /evidence/upnp-discovery-recovery.log; then break; fi
  sleep 0.1
done
grep -q NotFound /evidence/upnp-discovery-recovery.log
ip netns exec router node /lab/igd.mjs > /evidence/igd.log 2>&1 &
gateway_pid=$!
trap 'kill "$gateway_pid" 2>/dev/null || true' EXIT
for attempt in $(seq 1 30); do
  if grep -q 'IGD emulator ready' /evidence/igd.log; then break; fi
  sleep 0.1
done
if wait "$recovery_pid" && grep -q Active /evidence/upnp-discovery-recovery.log; then
  echo 'PASS initially missing gateway is rediscovered without restarting the mapper'
else
  echo 'FAIL delayed gateway recovery'; failures=$((failures+1))
fi
if ip netns exec client1 /bin/upnp_probe > /evidence/upnp.log 2>&1; then
  if grep -q '"action":"DeletePortMapping","previousClient":"10.99.0.2"' /evidence/igd.log; then
    echo 'PASS UPnP discovery/mapping/cleanup against emulator'
  else
    echo 'FAIL active UPnP mapping was not removed on shutdown'; failures=$((failures+1))
  fi
else
  echo 'FAIL UPnP mapping; see upnp.log and igd.log'; failures=$((failures+1))
fi
if bash /lab/upnp-lifecycle.sh; then
  echo 'PASS expanded UPnP lifecycle and real P2P forwarding'
  if bash /lab/upnp-native.sh; then
    echo 'PASS daemon UPnP integration and automatic discovery'
  else
    echo 'FAIL daemon UPnP integration'; failures=$((failures+1))
  fi
else
  echo 'FAIL expanded UPnP lifecycle'; failures=$((failures+1))
fi
curl -fsS --max-time 2 http://10.99.0.1:1901/test/cgnat >/dev/null
if ip netns exec client1 /bin/upnp_probe > /evidence/upnp-cgnat.log 2>&1; then
  echo 'FAIL CGNAT gateway reported as publicly reachable'; failures=$((failures+1))
elif grep -q 'UPnP state: NonRoutable' /evidence/upnp-cgnat.log; then
  echo 'PASS CGNAT gateway is reported as NonRoutable (not traversed)'
else
  echo 'FAIL CGNAT classification'; failures=$((failures+1))
fi
test "$failures" = 0
