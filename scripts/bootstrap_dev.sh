#!/usr/bin/env bash
set -euo pipefail
sudo apt-get update
sudo apt-get install -y build-essential pkg-config \
  libgtk-4-dev libadwaita-1-dev libgdk-pixbuf-2.0-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
if ! command -v cargo >/dev/null; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
# shellcheck disable=SC1091
source "$HOME/.cargo/env"
rustup default stable
pkg-config --modversion gtk4 libadwaita-1 gstreamer-1.0
cargo --version
