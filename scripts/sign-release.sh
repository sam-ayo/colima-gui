#!/bin/bash
# Signs the installer packages and the app archives in a directory, and
# writes latest.json there.
# The updater in the app reads latest.json (see src/updater.rs).
#
# Usage: scripts/sign-release.sh <directory> [notes file]
# The directory must have colima-gui-<version>-<arch>.pkg and
# colima-gui-<version>-<arch>.zip for arm64 and x86_64. The notes file has the text that the
# update alert shows.
# Needs: jq and OpenSSL 3. This script runs on macOS and on Linux.
#
# The signing key comes from UPDATE_SIGNING_KEY (the PEM text) or from
# UPDATE_SIGNING_KEY_FILE. The default file is
# ~/.config/colima-gui/update-signing-key.pem.

set -euo pipefail

REPO=sam-ayo/colima-gui-releases

root="$(cd "$(dirname "$0")/.." && pwd)"
dir="$1"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
notes="$(if [ -n "${2:-}" ]; then cat "$2"; fi)"

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

# Writes a JSON object with the URL and the signature of the file for each
# architecture. The updater finds the file by std::env::consts::ARCH, so the
# key for arm64 is aarch64.
sign() {
    local extension="$1" files='{}' arch file signature rust_arch url
    for arch in arm64 x86_64; do
        file="colima-gui-$version-$arch.$extension"
        if [ ! -f "$dir/$file" ]; then
            echo "$dir/$file is missing" >&2
            exit 1
        fi

        # The signed message is the version, a newline, and the file.
        # The base64 of Linux adds line breaks, so tr removes them.
        { printf '%s\n' "$version"; cat "$dir/$file"; } > "$work/message"
        signature="$("$openssl" pkeyutl -sign -rawin -inkey "$key" -in "$work/message" | base64 | tr -d '\n')"

        case "$arch" in arm64) rust_arch=aarch64 ;; *) rust_arch="$arch" ;; esac
        url="https://github.com/$REPO/releases/download/v$version/$file"
        files="$(jq --arg a "$rust_arch" --arg u "$url" --arg s "$signature" \
            '. + {($a): {url: $u, signature: $s}}' <<< "$files")"
    done
    printf '%s\n' "$files"
}

packages="$(sign pkg)"
apps="$(sign zip)"

jq -n --arg v "$version" --arg n "$notes" --argjson p "$packages" --argjson a "$apps" \
    '{version: $v, notes: $n, packages: $p, apps: $a}' > "$dir/latest.json"
