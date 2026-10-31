#!/usr/bin/env bash
set -euo pipefail
umask 077
script_path=$(realpath "$0")

phase=${BURST_CANDIDATE_PHASE:-all}
case "$phase" in all|wallet|node|preflight) ;; *) echo 'Choose all, wallet, node or preflight validation.' >&2; exit 2;; esac

test "${BURST_RELEASE_VALIDATION_HOST:-}" = 1 || exit 2
test "$(id -u)" -ne 0 || { echo 'Run apps as the validation user, not root.' >&2; exit 2; }
candidate=$(realpath "${1:?Candidate directory required}")
source_root=$(realpath "${2:?Matching source directory required}")
node_test_root=$(realpath "${BURST_NODE_TEST_ROOT:-$source_root/node-gui}")
test -f "$candidate/SHA256SUMS"
test -f "$candidate/SOURCES.sha256"
test -f "$source_root/wallet/tests/native-session.mjs"
test -f "$source_root/node-gui/tests/native-session.mjs"
cd "$candidate"
require_checksum() {
  awk -v path="$1" 'substr($0, 67) == path { matches++ } END { exit(matches != 1) }' SHA256SUMS || {
    echo "Candidate checksums must cover ${1#./} exactly once." >&2
    exit 2
  }
}
for metadata in network.txt SOURCES.sha256 toolchain.txt; do
  require_checksum "./$metadata"
done
sha256sum -c SHA256SUMS
mapfile -t network < "$candidate/network.txt"
test "${#network[@]}" -eq 2
network_kind=$(bash "$(dirname "$script_path")/candidate-identity.sh" "--${network[0]}" "${network[1]}")
if [[ $network_kind == private-testnet-fixture ]]; then
  test "${BURST_ALLOW_DISPOSABLE_FIXTURE:-}" = 1 || {
    echo 'Disposable fixture artifacts require explicit validation opt-in; never distribute them.' >&2
    exit 2
  }
fi
(cd "$source_root" && sha256sum --quiet -c "$candidate/SOURCES.sha256")
evidence=$(mktemp -d "$candidate/installation.XXXXXXXX")
echo "Installation evidence: $evidence"
exec > >(tee "$evidence/validation.log") 2>&1
sha256sum "$script_path" "$source_root/wallet/tests/native-session.mjs" \
  "$node_test_root/tests/native-session.mjs" "$node_test_root/tests/native-rpc.mjs" \
  "$node_test_root/tests/native-dashboard.mjs" > "$evidence/drivers.sha256"

require_checksum ./burst-daemon
test -x "$candidate/burst-daemon"
declare -A candidate_packages candidate_names candidate_binaries
for app in wallet node-gui; do
  mapfile -d '' packages < <(find "$candidate/$app" -maxdepth 1 -name '*.deb' -type f -print0)
  test "${#packages[@]}" -eq 1
  package=${packages[0]}
  require_checksum "./${package#"$candidate/"}"
  name=$(dpkg-deb -f "$package" Package)
  expected=burst-wallet
  binary=burst-wallet-gui
  if [[ $app == node-gui ]]; then expected=burst-node; binary=burst-node-gui; fi
  test "$name" = "$expected" || { echo "Unexpected $app package: $name" >&2; exit 2; }
  test "$(dpkg-deb -f "$package" Architecture)" = "$(dpkg --print-architecture)"
  version=$(node -e 'process.stdout.write(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).version)' "$source_root/$app/src-tauri/tauri.conf.json")
  test "$(dpkg-deb -f "$package" Version)" = "$version" || {
    echo "$app package version differs from its frozen source." >&2
    exit 2
  }
  node - "$source_root/$app/src-tauri/tauri.conf.json" "$(dpkg-deb -f "$package" Depends)" <<'NODE'
const fs = require('node:fs');
const config = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const required = config.bundle?.linux?.deb?.depends ?? [];
const normalize = value => value.replace(/\s+/g, ' ').trim();
const actual = process.argv[3].split(',').map(normalize);
if (!required.some(value => /^libc6\s*\(>=\s*[\d.]+\)$/.test(value)) ||
    required.some(value => !actual.includes(normalize(value)))) {
  console.error('Package runtime dependencies differ from its frozen source.');
  process.exit(2);
}
NODE
  dpkg-deb -x "$package" "$evidence/payload-$app"
  test -x "$evidence/payload-$app/usr/bin/$binary"
  candidate_packages[$app]=$package
  candidate_names[$app]=$name
  candidate_binaries[$app]=$binary
  if [[ $phase != preflight && $(dpkg-query -W -f '${db:Status-Status}' "$name" 2>/dev/null || true) == installed ]]; then
    test "${BURST_REUSE_INSTALLED:-}" = 1 || {
      echo "Refusing to replace existing package $name" >&2
      exit 2
    }
    test "$(dpkg-query -W -f '${Version}' "$name")" = "$(dpkg-deb -f "$package" Version)"
  fi
done
if [[ $phase == preflight ]]; then
  echo 'PASS artifact preflight only. No packages installed or application journeys run.'
  exit 0
fi

for app in wallet node-gui; do
  package=${candidate_packages[$app]}
  name=${candidate_names[$app]}
  binary=${candidate_binaries[$app]}
  if [[ $(dpkg-query -W -f '${db:Status-Status}' "$name" 2>/dev/null || true) != installed ]]; then
    dpkg-deb --info "$package"
    sudo -n apt-get install -y --no-remove --no-install-recommends "$package"
  fi
  dpkg-query -W -f '${Package} ${Version} ${Architecture} ${db:Status-Status}\n' "$name"
  cmp "$evidence/payload-$app/usr/bin/$binary" "/usr/bin/$binary"
done
test -x /usr/bin/burst-wallet-gui
test -x /usr/bin/burst-node-gui
sha256sum /usr/bin/burst-wallet-gui /usr/bin/burst-node-gui "$candidate/burst-daemon" > "$evidence/installed.sha256"

export TMPDIR="$evidence"
export GSETTINGS_BACKEND=memory LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe LP_NUM_THREADS=1
export TOKIO_WORKER_THREADS=1 RAYON_NUM_THREADS=1 UV_THREADPOOL_SIZE=1
export WALLET_BINARY=/usr/bin/burst-wallet-gui BURST_DAEMON="$candidate/burst-daemon"
if [[ $phase != node ]]; then
  timeout -k 10 900 node "$source_root/wallet/tests/native-session.mjs" --embedded
fi
if [[ $phase == wallet ]]; then
  echo 'PASS installed wallet journeys. Dashboard and certified Test acceptance remain separate.'
  exit 0
fi

test -z "$(ss -H -ltn 'sport = :32077')"
printf '%064d' 0 > "$evidence/public-dev.key"
"$candidate/burst-daemon" --network dev --data-dir "$evidence/node-ledger" --port 0 \
  --rpc-bind 127.0.0.1 --rpc-port 32077 --disable-upnp \
  --representative-key-file "$evidence/public-dev.key" node run > "$evidence/node.log" 2>&1 &
node_pid=$!
cleanup() {
  kill "$node_pid" 2>/dev/null || true
  wait "$node_pid" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
ready=false
for attempt in $(seq 1 60); do
  kill -0 "$node_pid"
  if curl -fsS --max-time 2 http://127.0.0.1:32077 -H 'Content-Type: application/json' \
    -d '{"action":"node_info"}' | jq -e '.result' >/dev/null; then ready=true; break; fi
  sleep 1
done
test "$ready" = true
NODE_BINARY=/usr/bin/burst-node-gui NODE_RPC_URL=http://127.0.0.1:32077 NODE_SELECTION_ENABLED=1 \
  timeout -k 10 300 node "$node_test_root/tests/native-session.mjs"
echo "PASS installed Linux $phase journeys. Certified Test, cross-installation and other release gates remain separate."
