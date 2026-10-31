#!/usr/bin/env bash
# Requires the live one-client lab and its saved success marker. Temporarily
# rejects P2P with TCP resets, checks disconnect/reconnect and runs the transport
# journey again. The EXIT trap removes the injected firewall rule.
set -euo pipefail
test "${BURST_ISOLATED_NAT_LAB:-}" = yes
test -f /evidence/one-client-passed
rpc() { curl -fsS --max-time 3 "http://$1:7077" -H 'Content-Type: application/json' -d '{"action":"peers_detailed"}'; }
ip netns exec router iptables -I FORWARD 1 -p tcp --dport 7076 -j REJECT --reject-with tcp-reset
cleanup() { ip netns exec router iptables -D FORWARD -p tcp --dport 7076 -j REJECT --reject-with tcp-reset 2>/dev/null || true; }
trap cleanup EXIT
disconnected=false
for attempt in $(seq 1 40); do
  if rpc 10.99.0.2 | jq -e '.result.count == 0' >/dev/null; then disconnected=true; break; fi
  sleep 1
done
test "$disconnected" = true
echo 'PASS client detects firewall-induced disconnect'
cleanup
reconnected=false
for attempt in $(seq 1 90); do
  if rpc 10.99.0.2 | jq -e '.result.count > 0' >/dev/null; then reconnected=true; break; fi
  sleep 1
done
test "$reconnected" = true
echo 'PASS client reconnects without restart after firewall rule is removed'
BURST_VALIDATION_REPORT=/evidence/reconnected-state.json /bin/validate_transport http://172.30.99.2:7077 http://10.99.0.2:7077
echo 'PASS new signed transactions propagate and confirm after reconnect'
touch /evidence/reconnect-passed
