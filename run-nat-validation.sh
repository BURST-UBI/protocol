#!/usr/bin/env bash
# Linux/Docker integration tests. No host firewall or Azure changes.
# Namespace setup needs a privileged container; it has NO external network,
# no Docker socket, no host network, and only the explicit mounts below.
set -euo pipefail
if [[ -z "${BURST_NAT_VALIDATION_HOST:-}" || "$BURST_NAT_VALIDATION_HOST" != "$(hostname)" ]]; then
  echo 'Run only on a provisioned validation host, with BURST_NAT_VALIDATION_HOST set to its hostname. Desktop release builds are prohibited.' >&2
  exit 2
fi
if (( $(awk '/^MemTotal:/ { print $2 }' /proc/meminfo) < 6 * 1024 * 1024 )); then
  echo 'The isolated NAT lab requires a validation host with at least 6 GiB RAM.' >&2
  exit 2
fi
if [[ -z ${BURST_NAT_ARTIFACTS:-} ]]; then
  echo 'Set BURST_NAT_ARTIFACTS to existing qualified binaries. Build separately under the validation resource limits; this runner never rebuilds.' >&2
  exit 2
fi
artifacts=$(realpath "$BURST_NAT_ARTIFACTS")
for binary in burst-daemon validate_transport upnp_probe; do
  test -x "$artifacts/$binary" || { echo "Missing executable: $artifacts/$binary" >&2; exit 2; }
done
lab_image=$(docker image inspect "${BURST_NAT_IMAGE:-burst-nat-lab:local}" --format '{{.Id}}') || {
  echo 'Provide an existing BURST_NAT_IMAGE; image preparation is a separate bounded stage.' >&2
  exit 2
}
[[ $lab_image =~ ^sha256:[a-f0-9]{64}$ ]] || { echo 'Invalid local lab image identity' >&2; exit 2; }
exec 9>/tmp/burst-nat-validation.lock
flock -n 9 || { echo 'Another NAT validation job is active on this host.' >&2; exit 2; }
cd "$(dirname "$0")"
lab_dir="$PWD/network/tests/nat"
evidence_root="${BURST_VALIDATION_ROOT:-$PWD/target/validation-evidence}"
mkdir -p "$evidence_root"
evidence=$(mktemp -d "$evidence_root/burst-nat.XXXXXXXX")
container="burst-nat-validation-$$"
echo "NAT evidence: $evidence"
cleanup() {
  if docker container inspect "$container" >/dev/null 2>&1; then
    docker logs "$container" > "$evidence/lab.log" 2>&1 || true
    docker rm -f "$container" >/dev/null
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
for binary in burst-daemon validate_transport upnp_probe; do
  cp "$artifacts/$binary" "$evidence/$binary"
  cmp "$artifacts/$binary" "$evidence/$binary"
done
sha256sum "$evidence/burst-daemon" "$evidence/validate_transport" "$evidence/upnp_probe" > "$evidence/binary-sha256.txt"
docker image inspect "$lab_image" --format '{{.Id}}' > "$evidence/lab-image.txt"
sha256sum "$0" "$lab_dir"/* > "$evidence/driver-sha256.txt"
docker run -d --name "$container" --network none --privileged \
  --memory 3g --memory-swap 3g --cpus 1 --pids-limit 256 \
  -e BURST_ISOLATED_NAT_LAB=yes -v "$lab_dir:/lab:ro" -v "$evidence:/evidence" \
  -v "$evidence/burst-daemon:/bin/burst-daemon:ro" \
  -v "$evidence/validate_transport:/bin/validate_transport:ro" \
  -v "$evidence/upnp_probe:/bin/upnp_probe:ro" \
  "$lab_image" timeout --signal=TERM --kill-after=10s 20m bash /lab/lab.sh >/dev/null
ready=false
for attempt in $(seq 1 240); do
  if test -f "$evidence/one-client-passed"; then ready=true; break; fi
  if test "$(docker inspect -f '{{.State.Running}}' "$container")" != true; then break; fi
  sleep 1
done
if test "$ready" != true; then docker logs "$container"; exit 1; fi
echo 'PASS single-client NAT and inbound firewall isolation'
failures=0
if docker exec "$container" bash /lab/reconnect.sh > "$evidence/reconnect.log" 2>&1; then
  echo 'PASS firewall interruption/reconnect and subsequent payments'
else
  echo "FAIL reconnect; see $evidence/reconnect.log"; failures=$((failures+1))
fi
if test -f "$evidence/reconnect-passed"; then
  if docker exec "$container" bash /lab/multiple-clients.sh > "$evidence/multiple-clients.log" 2>&1; then
    echo 'PASS multiple default-port clients behind one public IP'
  else
    echo "FAIL multiple NAT clients; see $evidence/multiple-clients.log"; failures=$((failures+1))
  fi
  if docker exec "$container" bash /lab/distinct-ports.sh > "$evidence/distinct-ports.log" 2>&1; then
    echo 'PASS distinct-port clients behind the same NAT'
  else
    echo "FAIL distinct-port clients; see $evidence/distinct-ports.log"; failures=$((failures+1))
  fi
else
  echo 'SKIP multiple-client test because reconnect prerequisite failed'
fi
if docker exec "$container" bash /lab/double-nat.sh > "$evidence/double-nat.log" 2>&1; then
  echo 'PASS two clients behind home NAT plus ISP CGNAT'
else
  echo "FAIL double NAT; see $evidence/double-nat.log"; failures=$((failures+1))
  echo 'SKIP manual forwarding and UPnP because single-router topology was not restored'
  exit 1
fi
if docker exec "$container" bash /lab/manual-forward.sh > "$evidence/manual-forward.log" 2>&1; then
  echo 'PASS inbound manual port forwarding and signed transactions'
else
  echo "FAIL manual forwarding; see $evidence/manual-forward.log"; failures=$((failures+1))
fi
if docker exec "$container" bash /lab/upnp-cases.sh > "$evidence/upnp-cases.log" 2>&1; then
  echo 'PASS UPnP mapping/cleanup and missing-gateway/CGNAT detection'
else
  echo "FAIL UPnP; see $evidence/upnp-cases.log, upnp.log and igd.log"; failures=$((failures+1))
fi
echo "NAT checks finished: $failures failure(s). Evidence retained: $evidence"
test "$failures" = 0
