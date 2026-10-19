#!/usr/bin/env bash
set -euo pipefail

# Run on a provisioned validation host. This builds artifacts, never publishes them.
test "${BURST_RELEASE_VALIDATION_HOST:-}" = 1 || {
  echo 'Set BURST_RELEASE_VALIDATION_HOST=1 on the validation host, not the desktop.' >&2
  exit 2
}
test "$(uname -s)" = Linux
: "${BURST_TEST_GENESIS_PUBLIC_KEY:?Provide the disposable network public key}"
test "$#" -eq 1
network_kind=$(bash "$(dirname "$0")/candidate-identity.sh" "$1" "$BURST_TEST_GENESIS_PUBLIC_KEY")
export BURST_TEST_GENESIS_PUBLIC_KEY=${BURST_TEST_GENESIS_PUBLIC_KEY,,}
: "${CARGO_TARGET_DIR:?Use an absolute build-cache directory}"
: "${BURST_CANDIDATE_PARENT:?Provide an existing output parent}"
test "${CARGO_TARGET_DIR:0:1}" = /
test -d "$BURST_CANDIDATE_PARENT"
source_root=$(cd "$(dirname "$0")/../.." && pwd)
for app in wallet node-gui; do
  test -f "$source_root/$app/package-lock.json"
  test -f "$source_root/$app/src-tauri/Cargo.lock"
done
test "$(df --output=avail -k "$BURST_CANDIDATE_PARENT" | tail -1)" -ge 8388608
test "$(awk '/MemAvailable/ {print $2}' /proc/meminfo)" -ge 6291456
output=$(mktemp -d "$BURST_CANDIDATE_PARENT/linux-candidate.XXXXXXXX")
echo "Candidate output: $output"
exec > >(tee "$output/build.log") 2>&1
export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0
export TOKIO_WORKER_THREADS=1 RAYON_NUM_THREADS=1 UV_THREADPOOL_SIZE=1
export RUSTUP_TOOLCHAIN=nightly-2026-10-11
rustc --version --verbose > "$output/toolchain.txt"
node --version >> "$output/toolchain.txt"
npm --version >> "$output/toolchain.txt"
printf '%s\n' "$network_kind" "$BURST_TEST_GENESIS_PUBLIC_KEY" > "$output/network.txt"
cd "$source_root"
find protocol wallet node-gui \
  -type d \( -name .git -o -name .cf2 -o -name .codex -o -name .claude -o -name target -o -name node_modules -o -name dist \) -prune -o \
  -type f ! -name '*.tsbuildinfo' ! -name '*.log' ! -name '.env' ! -name '.env.*' \
  ! -name 'validation-peer.env' ! -name '*.seed' ! -name '*.pem' ! -name '*.key' \
  ! -name 'id_rsa' ! -name 'id_ed25519' ! -name 'credentials' \
  -print0 | sort -z | xargs -0 sha256sum > "$output/SOURCES.sha256"

cd "$source_root/protocol"
timeout -k 10 1800 cargo build --release --locked -p burst-daemon --features private-testnet
cp "$CARGO_TARGET_DIR/release/burst-daemon" "$output/"
for app in wallet node-gui; do
  cd "$source_root/$app"
  timeout -k 10 300 npm ci --no-audit --no-fund
  timeout -k 10 1800 node_modules/.bin/tauri build --ci --bundles deb -- --locked
  mkdir "$output/$app"
  find "$CARGO_TARGET_DIR/release/bundle/deb" -maxdepth 1 -type f -name '*.deb' -print0 |
    while IFS= read -r -d '' package; do
      package_name=$(dpkg-deb -f "$package" Package)
      expected=burst-wallet
      if [[ $app == node-gui ]]; then expected=burst-node; fi
      if [[ $package_name == "$expected" ]]; then cp "$package" "$output/$app/"; fi
    done
  test "$(find "$output/$app" -name '*.deb' -type f | wc -l)" -eq 1
done
cd "$output"
find . -type f \( -name '*.deb' -o -name burst-daemon -o -name network.txt -o -name SOURCES.sha256 -o -name toolchain.txt \) -print0 |
  sort -z | xargs -0 sha256sum > SHA256SUMS
sha256sum -c SHA256SUMS
echo 'Build complete. Installation, native journeys and release approval remain required.'
