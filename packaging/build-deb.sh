#!/usr/bin/env bash
# Builds dist/awg-gui_<version>_<arch>.deb (or $OUTPUT)
# Run packaging/build-backend.sh first to embed amneziawg-go / awg / awg-quick
# (then the package needs no external AmneziaWG packages).
set -euo pipefail
umask 022

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ ! -f vendor/amneziawg-go || ! -f vendor/awg || ! -f vendor/awg-quick ]]; then
    echo "!! vendor/ is incomplete — the backend will NOT be embedded."
    echo "!! Run packaging/build-backend.sh first (or the package will need amneziawg-tools)."
    [[ "${REQUIRE_BACKEND:-0}" == 1 ]] && exit 1
fi

cargo build --release --locked
BIN=target/release/awg-gui
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
# Debian: "0.1.0-beta" -> "0.1.0~beta" (sorts before the final 0.1.0)
DEB_VERSION="${VERSION//-/\~}"
ARCH=$(dpkg --print-architecture)
PKG=dist/pkgroot
rm -rf "$PKG" && mkdir -p "$PKG/DEBIAN" dist

install -Dm755 "$BIN" "$PKG/usr/bin/awg-gui"
install -Dm644 packaging/awg-gui.desktop "$PKG/usr/share/applications/awg-gui.desktop"
install -Dm644 packaging/org.amnezia.awg-gui.policy "$PKG/usr/share/polkit-1/actions/org.amnezia.awg-gui.policy"
for s in 16 22 24 32 48 64 128 256 512; do
    mkdir -p "$PKG/usr/share/icons/hicolor/${s}x${s}/apps"
    "$BIN" --render-icon "$s" "$PKG/usr/share/icons/hicolor/${s}x${s}/apps/awg-gui.png"
done
sed -n '/^pub const ICON_SVG/,/^"##;/p' src/desktop.rs | sed '1s/.*r##"//; $d' \
    | install -Dm644 /dev/stdin "$PKG/usr/share/icons/hicolor/scalable/apps/awg-gui.svg"
install -Dm644 README.md "$PKG/usr/share/doc/awg-gui/README.md"
cat > "$PKG/usr/share/doc/awg-gui/copyright" <<'COPY'
awg-gui — AmneziaWG GUI client for Linux.

License: GNU General Public License v2 or later.
https://github.com/coinman-dev/awg-linux-client

The executable may embed (packaging/build-backend.sh):
  amneziawg-go     https://github.com/amnezia-vpn/amneziawg-go     MIT
  amneziawg-tools  https://github.com/amnezia-vpn/amneziawg-tools  GPL-2.0
  (awg, awg-quick; sources available at the URL above)
COPY

# Minimum glibc required by the binary and the embedded awg.
GLIBC=$( (objdump -T "$BIN"; [[ -f vendor/awg ]] && objdump -T vendor/awg) 2>/dev/null \
    | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1)
DEPENDS="libc6 (>= ${GLIBC:-2.34}), iproute2, bash, pkexec | policykit-1"
[[ -f vendor/awg && -f vendor/awg-quick ]] || DEPENDS="$DEPENDS, amneziawg-tools"
SIZE=$(du -sk "$PKG" | cut -f1)

cat > "$PKG/DEBIAN/control" <<CTRL
Package: awg-gui
Version: $DEB_VERSION
Architecture: $ARCH
Maintainer: coinman-dev <256808916+coinman-dev@users.noreply.github.com>
Installed-Size: $SIZE
Depends: $DEPENDS
Recommends: libegl1 | libgl1, systemd-resolved | resolvconf, nftables, wget | curl, xdg-utils
Suggests: amneziawg
Section: net
Priority: optional
Homepage: https://github.com/coinman-dev/awg-linux-client
Description: AmneziaWG VPN client with a graphical interface
 Manage AmneziaWG / WireGuard tunnels: import .conf / .zip files and
 AmneziaVPN vpn:// keys, connect and disconnect, see live statistics.
 Optional kill-switch, LAN bypass and restoring the tunnel at boot.
 Lives in the system tray; one self-contained executable with an embedded
 userspace AmneziaWG implementation (amneziawg-go) and awg / awg-quick.
CTRL

cat > "$PKG/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e
case "$1" in
    remove|purge)
        # "Restore the connection after reboot" creates this unit at runtime.
        if [ -f /etc/systemd/system/awg-gui-boot.service ]; then
            systemctl disable awg-gui-boot.service >/dev/null 2>&1 || true
            rm -f /etc/systemd/system/awg-gui-boot.service
            systemctl daemon-reload >/dev/null 2>&1 || true
        fi
        rm -rf /var/lib/awg-gui/boot
        ;;
esac
if [ "$1" = "purge" ]; then
    rm -rf /var/lib/awg-gui
fi
POSTRM
chmod 755 "$PKG/DEBIAN/postrm"

OUT="${OUTPUT:-dist/awg-gui_${VERSION}_${ARCH}.deb}"
mkdir -p "$(dirname "$OUT")"
dpkg-deb --root-owner-group -Zxz --build "$PKG" "$OUT" >/dev/null
rm -rf "$PKG"
echo ">> $OUT"
dpkg-deb --info "$OUT" | sed -n '/Package:/,$p'
dpkg-deb --contents "$OUT" | awk '{print $1, $6}'
