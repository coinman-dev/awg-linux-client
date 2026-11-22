#!/usr/bin/env bash
# Собирает бэкенд AmneziaWG для встраивания в awg-gui:
#   vendor/amneziawg-go  — userspace-реализация (Go, статически)
#   vendor/awg           — утилита настройки (C)
#   vendor/awg-quick     — скрипт поднятия туннеля (bash)
# Go при отсутствии скачивается во временный кэш. Затем: cargo build --release
# Builds the embedded AmneziaWG backend into vendor/ (Go is downloaded if missing).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/awg-gui-build"
GO_VERSION="${GO_VERSION:-1.24.6}"
# Pinned releases (reproducible builds); override to test newer ones.
GO_REF="${AWG_GO_REF:-v3.1.20261116}"
TOOLS_REF="${AWG_TOOLS_REF:-v3.1.20260812}"
mkdir -p "$CACHE" "$ROOT/vendor"

fetch() { # url dir ref
    if [[ -d "$2/.git" ]]; then
        git -C "$2" fetch -q --depth 1 origin "$3" && git -C "$2" checkout -q --force FETCH_HEAD
    else
        git clone -q --depth 1 --branch "$3" "$1" "$2"
    fi
}

# ---- amneziawg-go ----
if command -v go >/dev/null 2>&1; then
    GO=go
else
    case "$(uname -m)" in
        x86_64) GOARCH=amd64 ;;
        aarch64) GOARCH=arm64 ;;
        *) echo "unsupported arch $(uname -m)"; exit 1 ;;
    esac
    if [[ ! -x "$CACHE/go/bin/go" ]]; then
        echo ">> downloading Go $GO_VERSION"
        curl -fsSL "https://go.dev/dl/go${GO_VERSION}.linux-${GOARCH}.tar.gz" -o "$CACHE/go.tgz"
        rm -rf "$CACHE/go" && tar -C "$CACHE" -xzf "$CACHE/go.tgz" && rm "$CACHE/go.tgz"
    fi
    GO="$CACHE/go/bin/go"
fi
fetch https://github.com/amnezia-vpn/amneziawg-go "$CACHE/amneziawg-go" "$GO_REF"
echo ">> building amneziawg-go ($(git -C "$CACHE/amneziawg-go" rev-parse --short HEAD))"
( cd "$CACHE/amneziawg-go" && GOTOOLCHAIN=auto GOFLAGS=-trimpath CGO_ENABLED=0 \
    "$GO" build -ldflags "-s -w" -o "$ROOT/vendor/amneziawg-go" . )

# ---- amneziawg-tools ----
fetch https://github.com/amnezia-vpn/amneziawg-tools "$CACHE/amneziawg-tools" "$TOOLS_REF"
echo ">> building awg ($(git -C "$CACHE/amneziawg-tools" rev-parse --short HEAD))"
make -s -C "$CACHE/amneziawg-tools/src" clean >/dev/null 2>&1 || true
make -s -C "$CACHE/amneziawg-tools/src" >/dev/null 2>&1
install -m 755 "$CACHE/amneziawg-tools/src/wg" "$ROOT/vendor/awg"
strip "$ROOT/vendor/awg"
install -m 755 "$CACHE/amneziawg-tools/src/wg-quick/linux.bash" "$ROOT/vendor/awg-quick"

{
    echo "amneziawg-go $GO_REF ($(git -C "$CACHE/amneziawg-go" rev-parse --short HEAD))"
    echo "amneziawg-tools $TOOLS_REF ($(git -C "$CACHE/amneziawg-tools" rev-parse --short HEAD))"
} > "$ROOT/vendor/versions.txt"
cat "$ROOT/vendor/versions.txt"
ls -la "$ROOT/vendor"
