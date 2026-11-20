#!/usr/bin/env bash
set -euo pipefail

# Build ph-tui and install it to a prefix (default: /usr/local).
# Usage: ./install.sh [--prefix /usr/local] [--uninstall]

PREFIX="/usr/local"
UNINSTALL=0

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix)
            PREFIX="${2:?--prefix needs a value}"
            shift 2
            ;;
        --prefix=*)
            PREFIX="${1#*=}"
            shift
            ;;
        --uninstall)
            UNINSTALL=1
            shift
            ;;
        -h|--help)
            sed -n '3,5p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            exit 1
            ;;
    esac
done

BIN="$PREFIX/bin/ph-tui"

if [ "$UNINSTALL" -eq 1 ]; then
    rm -f "$BIN"
    echo "removed $BIN"
    exit 0
fi

for tool in mpv yt-dlp; do
    command -v "$tool" >/dev/null || echo "warning: $tool is not installed" >&2
done

SCRIPT_DIR="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if ! command -v cargo >/dev/null; then
    echo "cargo not found; install Rust from https://rustup.rs" >&2
    exit 1
fi

cargo build --release

if [ -w "$(dirname "$BIN")" ]; then
    install -Dm755 target/release/ph-tui "$BIN"
else
    sudo install -Dm755 target/release/ph-tui "$BIN"
fi

echo "installed $BIN"
echo "run: ph-tui"
