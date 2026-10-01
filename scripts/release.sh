#!/bin/bash
# Builds the release files that the updater in the app reads (see
# src/updater.rs): one signed installer package for each architecture, and
# latest.json. The release workflow does not use this script. It builds each
# architecture in a different job, and then runs scripts/sign-release.sh.
#
# Usage: scripts/release.sh [notes file]
# The notes file has the text that the update alert shows.
# Output: target/release-files/
# Needs: the needs of scripts/package.sh and scripts/sign-release.sh, and
# rustup with the aarch64-apple-darwin and x86_64-apple-darwin targets.
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

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
out="$target/release-files"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

rm -rf "$out"
mkdir -p "$out"

for arch in arm64 x86_64; do
    "$root/scripts/package.sh" "$arch"
    file="colima-gui-$version-$arch.pkg"
    cp "$target/package/$arch/$file" "$out/$file"
done

"$root/scripts/sign-release.sh" "$out" "${1:-}"

echo
echo "release files: $out"
ls -l "$out"
