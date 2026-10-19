#!/usr/bin/env bash
# Isolate Playwright WebKit 2272's Ubuntu ABI dependencies on this Arch host.
# No package installation, maintainer scripts, or edits to the source cache.
set -euo pipefail

if [[ $# != 1 || "$1" != /* || ! -f "$1/pw_run.sh" || $(basename -- "$1") != webkit-2272 ]]; then
    echo "Usage: bash tools/prepare-webkit-compat.sh /absolute/cache/webkit-2272" >&2
    exit 2
fi
for command_name in curl bsdtar sha256sum cp mktemp realpath; do
    command -v "$command_name" >/dev/null
done
browser_source=$(realpath -- "$1")
compat_dir=$(mktemp -d /tmp/burst-webkit-compat.XXXXXX)
echo "Preparing isolated browser in $compat_dir" >&2
mkdir "$compat_dir/root" "$compat_dir/browsers"

fetch_library() {
    local filename=$1 url=$2 checksum=$3
    curl --fail --location --proto '=https' --proto-redir '=https' --silent --show-error --max-time 90 \
        "$url" -o "$compat_dir/$filename"
    printf '%s  %s\n' "$checksum" "$compat_dir/$filename" | sha256sum -c - >&2
    bsdtar -xOf "$compat_dir/$filename" data.tar.zst | bsdtar -xf - -C "$compat_dir/root"
}

# Sources and SHA256 values were checked on packages.ubuntu.com on 2026-10-11.
# https://packages.ubuntu.com/noble-updates/amd64/libicu74/download
fetch_library libicu74.deb \
    https://archive.ubuntu.com/ubuntu/pool/main/i/icu/libicu74_74.2-1ubuntu3.1_amd64.deb \
    c9a70989678660eed9a1e904c74fa043da8bec8e2036856fc16e31ced79b04f8
# https://packages.ubuntu.com/noble/amd64/libxml2/download
fetch_library libxml2.deb \
    https://security.ubuntu.com/ubuntu/pool/main/libx/libxml2/libxml2_2.9.14+dfsg-1.3ubuntu3.10_amd64.deb \
    c5a065f85c1e6f605741e365e40cad71c3ceb0e85e2791400bd35b8896e12ad1
# https://packages.ubuntu.com/noble/amd64/libflite1/download
fetch_library libflite1.deb \
    https://archive.ubuntu.com/ubuntu/pool/universe/f/flite/libflite1_2.2-6build3_amd64.deb \
    367f1d0da5cd38759a0515eafc27aa133b2d7bf99308cac34831df0212e96b75

cp --reflink=auto -a "$browser_source" "$compat_dir/browsers/webkit-2272"
for platform in minibrowser-gtk minibrowser-wpe; do
    library_dir=$(realpath -- "$compat_dir/browsers/webkit-2272/$platform/sys/lib")
    if [[ "$library_dir" != "$compat_dir/browsers/webkit-2272/"* ]]; then
        echo "Refusing to write libraries outside the isolated browser" >&2
        exit 1
    fi
    cp -a "$compat_dir/root/usr/lib/x86_64-linux-gnu/." \
        "$library_dir/"
done
# Set PLAYWRIGHT_BROWSERS_PATH to this output for the fixture test command.
printf '%s\n' "$compat_dir/browsers"
