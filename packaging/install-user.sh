#!/bin/sh
# Build and install wavelinkd, wavectl and the wavelink UI for the current user.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
bin="$HOME/.local/bin"; share="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm755 target/release/wavelinkd target/release/wavectl target/release/wavelink -t "$bin"
install -Dm644 packaging/wavelink.desktop -t "$share/applications"
install -Dm644 packaging/wavelink.svg -t "$share/icons/hicolor/scalable/apps"
install -Dm644 systemd/wavelinkd.service -t "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
update-desktop-database "$share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$share/icons/hicolor" 2>/dev/null || true
command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
systemctl --user daemon-reload
systemctl --user try-restart wavelinkd.service
echo "installed to $bin"
