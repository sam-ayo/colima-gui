#!/bin/bash
# Builds "Colima.app" and an installer package (.pkg) for macOS.
#
# The app contains the Colima, Lima, and Docker command line tools. The
# installer puts the app in /Applications. It then links the tools into
# /usr/local/bin, but only for tools that are not already installed.
#
# Usage: scripts/package.sh [arm64 | x86_64]
# The default architecture is the architecture of this Mac. For the other
# architecture, first run `rustup target add <rust target>`.
# Output: target/package/<arch>/Colima.app and
#         target/package/<arch>/colima-gui-<version>-<arch>.pkg
# Needs: cargo, curl, and the Xcode command line tools (pkgbuild).

set -euo pipefail

COLIMA_VERSION=0.10.3
LIMA_VERSION=2.2.0
DOCKER_VERSION=29.8.1

APP_NAME=Colima
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

# Lima finds its guest agents in ../share/lima, next to bin/limactl. Keep the
# layout of the archive. Do not sign the tools again: limactl has the
# virtualization entitlement, and a new signature removes it.
tar xzf "$cache/$lima_file" -C "$tools" ./bin ./libexec ./share/lima
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
tools="/Applications/Colima.app/Contents/Resources/tools/bin"
mkdir -p /usr/local/bin
for tool in colima limactl lima docker; do
    link="/usr/local/bin/$tool"
    if [ -L "$link" ] && [[ "$(readlink "$link")" == "$tools/"* ]]; then
        ln -sf "$tools/$tool" "$link"
    elif [ ! -e "$link" ] && [ ! -e "/opt/homebrew/bin/$tool" ]; then
        ln -s "$tools/$tool" "$link"
    fi
done
exit 0
EOF
chmod 755 "$scripts/postinstall"

# The app bundle must stay at /Applications/Colima.app, because the links
# point there. Do not let the installer move it to a copy in another place.
component_plist="$cache/component-$arch.plist"
pkgbuild --analyze --root "$pkgroot" "$component_plist" >/dev/null
plutil -replace 0.BundleIsRelocatable -bool NO "$component_plist"

pkg="$out/colima-gui-$version-$arch.pkg"
pkgbuild \
    --root "$pkgroot" \
    --component-plist "$component_plist" \
    --scripts "$scripts" \
    --identifier "$BUNDLE_ID.pkg" \
    --version "$version" \
    --install-location / \
    "$pkg"

echo
echo "app: $app"
echo "installer: $pkg"
