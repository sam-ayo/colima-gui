#!/bin/bash
# Builds the release files that the updater in the app reads (see
# src/updater.rs): one signed installer package for each architecture, and
# latest.json.
#
# Usage: scripts/release.sh [notes file]
# The notes file has the text that the update alert shows.
# Output: target/release-files/
# Needs: the needs of scripts/package.sh, rustup with the aarch64-apple-darwin
# and x86_64-apple-darwin targets, jq, and OpenSSL 3.
#
# The signing key comes from UPDATE_SIGNING_KEY (the PEM text) or from
# UPDATE_SIGNING_KEY_FILE. The default file is
# ~/.config/colima-gui/update-signing-key.pem.
#
# To make a new key, run:
#   openssl genpkey -algorithm ed25519 -out update-signing-key.pem
# Then put the public key in PUBLIC_KEY in src/updater.rs. To get it, run:
#   openssl pkey -in update-signing-key.pem -pubout -outform DER | tail -c 32 | base64
# An app with the old public key cannot install updates from the new key.

set -euo pipefail

REPO=sam-ayo/colima-gui-releases

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
out="$target/release-files"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
notes="$(if [ -n "${1:-}" ]; then cat "$1"; fi)"

# The OpenSSL of macOS is LibreSSL, which cannot sign with Ed25519.
openssl=openssl
if [ -x "$(brew --prefix 2>/dev/null)/opt/openssl@3/bin/openssl" ]; then
    openssl="$(brew --prefix)/opt/openssl@3/bin/openssl"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
key="$work/key.pem"
(
    umask 077
    if [ -n "${UPDATE_SIGNING_KEY:-}" ]; then
        printf '%s\n' "$UPDATE_SIGNING_KEY" > "$key"
    else
        cp "${UPDATE_SIGNING_KEY_FILE:-$HOME/.config/colima-gui/update-signing-key.pem}" "$key"
    fi
)

rm -rf "$out"
mkdir -p "$out"

# The updater finds the package by std::env::consts::ARCH, so the key for
# arm64 is aarch64.
packages='{}'
for arch in arm64 x86_64; do
    "$root/scripts/package.sh" "$arch"
    file="colima-gui-$version-$arch.pkg"
    cp "$target/package/$arch/$file" "$out/$file"

    # The signed message is the version, a newline, and the package.
    { printf '%s\n' "$version"; cat "$out/$file"; } > "$work/message"
    signature="$("$openssl" pkeyutl -sign -rawin -inkey "$key" -in "$work/message" | base64)"

    case "$arch" in arm64) rust_arch=aarch64 ;; *) rust_arch="$arch" ;; esac
    url="https://github.com/$REPO/releases/download/v$version/$file"
    packages="$(jq --arg a "$rust_arch" --arg u "$url" --arg s "$signature" \
        '. + {($a): {url: $u, signature: $s}}' <<< "$packages")"
done

jq -n --arg v "$version" --arg n "$notes" --argjson p "$packages" \
    '{version: $v, notes: $n, packages: $p}' > "$out/latest.json"

echo
echo "release files: $out"
ls -l "$out"
