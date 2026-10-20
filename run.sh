#!/usr/bin/env bash
# Jalankan Cegat (desktop app).
#   ./run.sh          -> mode dev (hot reload)
#   ./run.sh build    -> bundle release (.app/.dmg/.msi/.deb)
set -euo pipefail
cd "$(dirname "$0")"

# rustup menaruh cargo di ~/.cargo; shell non-login kadang belum load PATH-nya.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "✗ '$1' tidak ditemukan. $2" >&2; exit 1; }
}
need cargo "Install Rust: https://rustup.rs"
need node "Install Node.js 22.12+: https://nodejs.org"
need npm "Install Node.js 22.12+: https://nodejs.org"

if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then
  echo "→ npm install"
  npm install
fi

case "${1:-dev}" in
  dev)
    exec npm run tauri dev
    ;;
  build)
    npm run tauri build
    echo "✓ Hasil bundle: src-tauri/target/release/bundle/"
    ;;
  *)
    echo "Pakai: ./run.sh [dev|build]" >&2
    exit 1
    ;;
esac
