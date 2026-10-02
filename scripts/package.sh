#!/bin/bash
# Builds "Colima GUI.app" and an installer package (.pkg) for macOS.
#
# The app contains the Colima, Lima, and Docker command line tools. The
# installer puts the app in /Applications. It then links the tools into
# /usr/local/bin, but only for tools that are not already installed.
#
# Usage: scripts/package.sh [arm64 | x86_64]
# The default architecture is the architecture of this Mac. For the other
# architecture, first run `rustup target add <rust target>`.
# Output: "target/package/<arch>/Colima GUI.app",
#         target/package/<arch>/colima-gui-<version>-<arch>.pkg, and
#         target/package/<arch>/colima-gui-<version>-<arch>.zip
# Needs: cargo, curl, and the Xcode command line tools (pkgbuild).
#
# Signature and notarization. Without these variables, the script makes an
# unsigned package, and Gatekeeper blocks it on other Macs.
#   CODESIGN_IDENTITY: the "Developer ID Application" certificate, for
#     example "Developer ID Application: Name (TEAMID)".
#   INSTALLER_IDENTITY: the "Developer ID Installer" certificate.
#   Notarization needs INSTALLER_IDENTITY and one of these:
#     NOTARY_PROFILE: a keychain profile from `xcrun notarytool
#       store-credentials`.
#     NOTARY_KEY_FILE, NOTARY_KEY_ID, and NOTARY_ISSUER: an App Store
#       Connect API key (the .p8 file), its key ID, and its issuer ID.

set -euo pipefail

COLIMA_VERSION=0.10.3
LIMA_VERSION=2.2.0
DOCKER_VERSION=29.8.1

APP_NAME="Colima GUI"
BUNDLE_ID=colima-gui

arch="${1:-$(uname -m)}"
case "$arch" in
    arm64) rust_target=aarch64-apple-darwin; colima_arch=arm64; lima_arch=arm64; docker_arch=aarch64 ;;
    x86_64) rust_target=x86_64-apple-darwin; colima_arch=x86_64; lima_arch=x86_64; docker_arch=x86_64 ;;
    *) echo "unknown architecture: $arch" >&2; exit 1 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
target="${CARGO_TARGET_DIR:-$root/target}"
out="$target/package/$arch"
cache="$target/package-cache"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

# Downloads a file one time into the cache. The second argument is the file
# name in the cache.
fetch() {
    local url="$1" file="$cache/$2"
    if [ ! -f "$file" ]; then
        echo "download $url"
        curl -fsSL -o "$file.part" "$url"
        mv "$file.part" "$file"
    fi
}

# Makes sure that a file has the SHA-256 sum in a sums file.
verify() {
    local file="$1" sums="$2" name="$3"
    local want got
    want="$(awk -v n="$name" '$2 == n || $2 == "*"n {print $1}' "$sums")"
    got="$(shasum -a 256 "$file" | awk '{print $1}')"
    if [ -z "$want" ] || [ "$want" != "$got" ]; then
        echo "checksum does not agree for $name" >&2
        exit 1
    fi
}

mkdir -p "$cache"
rm -rf "$out"
mkdir -p "$out"

echo "build colima-gui $version for $arch"
cargo build --release --target "$rust_target" --manifest-path "$root/Cargo.toml"

colima_file="colima-Darwin-$colima_arch"
colima_url="https://github.com/abiosoft/colima/releases/download/v$COLIMA_VERSION"
fetch "$colima_url/$colima_file" "$colima_file-$COLIMA_VERSION"
fetch "$colima_url/$colima_file.sha256sum" "$colima_file-$COLIMA_VERSION.sha256sum"
verify "$cache/$colima_file-$COLIMA_VERSION" "$cache/$colima_file-$COLIMA_VERSION.sha256sum" "$colima_file"

lima_file="lima-$LIMA_VERSION-Darwin-$lima_arch.tar.gz"
lima_url="https://github.com/lima-vm/lima/releases/download/v$LIMA_VERSION"
fetch "$lima_url/$lima_file" "$lima_file"
fetch "$lima_url/SHA256SUMS" "lima-$LIMA_VERSION-SHA256SUMS"
verify "$cache/$lima_file" "$cache/lima-$LIMA_VERSION-SHA256SUMS" "$lima_file"

# Docker does not publish sums for the static files. HTTPS is the check.
docker_file="docker-$DOCKER_VERSION.tgz"
fetch "https://download.docker.com/mac/static/stable/$docker_arch/$docker_file" "docker-$docker_arch-$DOCKER_VERSION.tgz"

app="$out/$APP_NAME.app"
tools="$app/Contents/Resources/tools"
mkdir -p "$app/Contents/MacOS" "$tools/bin"

cp "$target/$rust_target/release/colima-gui" "$app/Contents/MacOS/colima-gui"
cp "$root/assets/icon/colima-gui.icns" "$app/Contents/Resources/colima-gui.icns"

# Lima finds its guest agents in ../share/lima, next to bin/limactl. Keep the
# layout of the archive. Leave out the guest agent for macOS guests, because
# Colima runs only Linux guests. It is an unsigned binary in a gzip file, and
# notarization rejects it.
tar xzf "$cache/$lima_file" -C "$tools" --exclude './share/lima/lima-guestagent.Darwin-*' \
    ./bin ./libexec ./share/lima
install -m 755 "$cache/$colima_file-$COLIMA_VERSION" "$tools/bin/colima"
tar xzf "$cache/docker-$docker_arch-$DOCKER_VERSION.tgz" -C "$cache" docker/docker
install -m 755 "$cache/docker/docker" "$tools/bin/docker"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleExecutable</key>
    <string>colima-gui</string>
    <key>CFBundleIconFile</key>
    <string>colima-gui</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

# Notarization needs a Developer ID signature, the hardened runtime, and a
# secure timestamp on all of the code. The tools have ad hoc signatures, so
# sign them again. limactl has the virtualization entitlement, and a new
# signature removes it. Thus copy the entitlements of each tool to its new
# signature. Sign the bundle last, because its seal includes the tools.
if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    echo "sign the app with $CODESIGN_IDENTITY"
    entitlements="$cache/entitlements.plist"
    find "$tools" -type f -perm -u+x | while read -r file; do
        if ! file -b "$file" | grep -q Mach-O; then
            continue
        fi
        rm -f "$entitlements"
        codesign -d --entitlements - --xml "$file" > "$entitlements" 2>/dev/null || true
        args=()
        if [ -s "$entitlements" ]; then
            args=(--entitlements "$entitlements")
        fi
        codesign --force --options runtime --timestamp \
            --sign "$CODESIGN_IDENTITY" "${args[@]+"${args[@]}"}" "$file"
    done
    codesign --force --options runtime --timestamp \
        --sign "$CODESIGN_IDENTITY" "$app"
    codesign --verify --deep --strict "$app"
fi

# The updater replaces an installed app with the app in this archive, and
# does not need a password (see src/updater.rs).
ditto -c -k --keepParent "$app" "$out/colima-gui-$version-$arch.zip"

# The installer package. The postinstall script links the tools.
pkgroot="$cache/pkgroot-$arch"
scripts="$cache/pkgscripts-$arch"
rm -rf "$pkgroot" "$scripts"
mkdir -p "$pkgroot/Applications" "$scripts"
cp -R "$app" "$pkgroot/Applications/"

cat > "$scripts/postinstall" <<'EOF'
#!/bin/bash
# Links the tools in the app into /usr/local/bin. A tool that is already
# installed (for example from Homebrew) stays as it is.
app="/Applications/Colima GUI.app"
tools="$app/Contents/Resources/tools/bin"
# Versions 0.1.5 and earlier used the name "Colima.app". Remove that app, and
# move its links to the new app.
old="/Applications/Colima.app"
old_tools="$old/Contents/Resources/tools/bin"
if [ "$(defaults read "$old/Contents/Info" CFBundleIdentifier 2>/dev/null)" = colima-gui ]; then
    rm -rf "$old"
fi
mkdir -p /usr/local/bin
for tool in colima limactl lima docker; do
    link="/usr/local/bin/$tool"
    target="$(readlink "$link" 2>/dev/null || true)"
    if [ -L "$link" ] && { [[ "$target" == "$tools/"* ]] || [[ "$target" == "$old_tools/"* ]]; }; then
        ln -sf "$tools/$tool" "$link"
    elif [ ! -e "$link" ] && [ ! -e "/opt/homebrew/bin/$tool" ]; then
        ln -s "$tools/$tool" "$link"
    fi
done
# Give the app to the user, so that the updater can replace it without an
# administrator password. The user is the owner of the console, because the
# installer runs this script as root.
user="$(stat -f %Su /dev/console)"
if [ -n "$user" ] && [ "$user" != root ] && id "$user" >/dev/null 2>&1; then
    chown -R "$user" "$app"
fi
exit 0
EOF
chmod 755 "$scripts/postinstall"

# The app bundle must stay at "/Applications/Colima GUI.app", because the links
# point there. Do not let the installer move it to a copy in another place.
component_plist="$cache/component-$arch.plist"
pkgbuild --analyze --root "$pkgroot" "$component_plist" >/dev/null
plutil -replace 0.BundleIsRelocatable -bool NO "$component_plist"

sign_args=()
if [ -n "${INSTALLER_IDENTITY:-}" ]; then
    sign_args=(--sign "$INSTALLER_IDENTITY" --timestamp)
fi

pkg="$out/colima-gui-$version-$arch.pkg"
pkgbuild \
    --root "$pkgroot" \
    --component-plist "$component_plist" \
    --scripts "$scripts" \
    --identifier "$BUNDLE_ID.pkg" \
    --version "$version" \
    --install-location / \
    "${sign_args[@]+"${sign_args[@]}"}" \
    "$pkg"

# Apple examines the package and the app in it. The staple step attaches the
# ticket to the package, so that Gatekeeper can accept it without a network
# connection. The update signature (scripts/sign-release.sh) must come after
# this step, because the staple step changes the package.
notary_args=()
if [ -n "${NOTARY_PROFILE:-}" ]; then
    notary_args=(--keychain-profile "$NOTARY_PROFILE")
elif [ -n "${NOTARY_KEY_FILE:-}" ]; then
    notary_args=(--key "$NOTARY_KEY_FILE" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
fi
if [ -n "${INSTALLER_IDENTITY:-}" ] && [ ${#notary_args[@]} -gt 0 ]; then
    echo "notarize $pkg"
    xcrun notarytool submit "$pkg" "${notary_args[@]}" --wait --timeout 1h \
        --output-format json > "$cache/notary-$arch.json" || true
    status="$(plutil -extract status raw "$cache/notary-$arch.json" 2>/dev/null || true)"
    if [ "$status" != "Accepted" ]; then
        cat "$cache/notary-$arch.json" >&2
        id="$(plutil -extract id raw "$cache/notary-$arch.json" 2>/dev/null || true)"
        if [ -n "$id" ]; then
            xcrun notarytool log "$id" "${notary_args[@]}" >&2 || true
        fi
        echo "notarization failed" >&2
        exit 1
    fi
    xcrun stapler staple "$pkg"
    spctl --assess --type install --verbose "$pkg"
fi

echo
echo "app: $app"
echo "installer: $pkg"
echo "archive: $out/colima-gui-$version-$arch.zip"
