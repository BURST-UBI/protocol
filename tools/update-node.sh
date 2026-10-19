#!/bin/sh
set -eu

RELEASE_URL="https://github.com/BURST-UBI/protocol/releases/latest/download"

download_verified_binary() (
    set -eu
    destination=$1
    asset=$2
    stage=$(mktemp -d "${destination}.download.XXXXXX") || exit 1
    trap 'rm -f "$stage/binary" "$stage/SHA256SUMS"; rmdir "$stage"' EXIT
    trap 'exit 1' HUP INT TERM
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 10 --max-time 180 --retry 2 \
        -o "$stage/SHA256SUMS" "${RELEASE_URL}/SHA256SUMS" || exit 1
    expected=$(awk -v asset="$asset" '
        $2 == asset {
            count++
            if (NF != 2 || length($1) != 64 || $1 ~ /[^[:xdigit:]]/) invalid = 1
            hash = tolower($1)
        }
        END { if (count != 1 || invalid) exit 1; print hash }
    ' "$stage/SHA256SUMS") || { echo 'Missing, duplicate or invalid release checksum' >&2; exit 1; }
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 10 --max-time 180 --retry 2 \
        -o "$stage/binary" "${RELEASE_URL}/${asset}" || exit 1
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$stage/binary" | awk '{ print $1 }')
    else
        actual=$(shasum -a 256 "$stage/binary" | awk '{ print $1 }')
    fi
    [ "$actual" = "$expected" ] || { echo 'Release checksum mismatch; installed binary was not changed' >&2; exit 1; }
    chmod 755 "$stage/binary" || exit 1
    mv -f "$stage/binary" "$destination"
)

# Arguments are separate service-manager entries, never interpolated shell code.
[ "$#" -eq 3 ] || { echo 'Usage: burst-update <binary> <platform-arch> <service>' >&2; exit 2; }
binary=$1
platform=$2
service=$3
case "$platform" in
    linux-amd64|linux-arm64) manager=systemd ;;
    darwin-amd64|darwin-arm64) manager=launchd ;;
    *) echo 'Unsupported update platform' >&2; exit 2 ;;
esac
case "$binary" in /*) ;; *) echo 'Binary path must be absolute' >&2; exit 2 ;; esac
[ -f "$binary" ] && [ ! -L "$binary" ] || { echo 'Installed binary is missing or is a symlink' >&2; exit 1; }
lock="${binary}.update-lock"
if ! mkdir "$lock" 2>/dev/null; then
    echo "Update lock unavailable: $lock. Check for a running updater before removing a stale lock." >&2
    exit 1
fi
trap 'rm -f "$lock/candidate"; rmdir "$lock"' EXIT
trap 'exit 1' HUP INT TERM
download_verified_binary "$lock/candidate" "burst-daemon-$platform"
if cmp -s "$binary" "$lock/candidate"; then exit 0; fi

# Retain the old executable for diagnosis. Rolling a binary back does not roll
# back a ledger schema, so a failed new service requires operator attention.
backup=$(mktemp "${binary}.previous.XXXXXX")
cp -p "$binary" "$backup"
mv -f "$lock/candidate" "$binary"
printf 'Installed verified update; previous executable retained at %s\n' "$backup"
if [ "$manager" = systemd ]; then
    # Updating a deliberately stopped service must not start it.
    if systemctl is-active --quiet "$service"; then systemctl restart "$service"; fi
else
    domain="gui/$(id -u)/$service"
    # A loaded launchd job may be stopped. Only a positive PID for this exact
    # label authorizes a restart; registration alone must not start the node.
    if jobs=$(launchctl list 2>/dev/null) && printf '%s\n' "$jobs" | awk -v label="$service" '
        NF == 3 && $3 == label && $1 ~ /^[0-9]+$/ && $1 > 0 { running = 1 }
        END { exit !running }
    '; then
        launchctl kickstart -k "$domain"
    fi
fi
