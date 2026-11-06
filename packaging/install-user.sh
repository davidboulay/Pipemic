#!/bin/sh
# Build and install pipemicd, pipemicctl and the pipemic UI for the current user.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
bin="$HOME/.local/bin"; share="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm755 target/release/pipemicd target/release/pipemicctl target/release/pipemic target/release/pipemic-login -t "$bin"
install -Dm644 packaging/pipemic.desktop -t "$share/applications"
install -Dm644 packaging/pipemic.svg packaging/pipemic-muted.svg -t "$share/icons/hicolor/scalable/apps"
# PNGs for every standard size; small sizes use the simplified drawing.
if command -v rsvg-convert >/dev/null; then
  for n in 16 22 24 32 48 64 128 256 512; do
    src=packaging/pipemic.svg; [ "$n" -le 32 ] && src=packaging/pipemic-small.svg
    mkdir -p "$share/icons/hicolor/${n}x${n}/apps"
    rsvg-convert -w "$n" -h "$n" "$src" -o "$share/icons/hicolor/${n}x${n}/apps/pipemic.png"
    rsvg-convert -w "$n" -h "$n" packaging/pipemic-muted.svg -o "$share/icons/hicolor/${n}x${n}/apps/pipemic-muted.png"
  done
fi
mkdir -p "$share/pipemic" && pwd > "$share/pipemic/source"   # for the in-app update check
install -Dm644 systemd/pipemicd.service -t "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
update-desktop-database "$share/applications" 2>/dev/null || true
# Marketplace "Open with Wave Link" links open Pipemic (no Wave Link on Linux).
xdg-mime default pipemic.desktop x-scheme-handler/wavelink x-scheme-handler/wavelink-v2 >/dev/null 2>&1 || true
gtk-update-icon-cache -q "$share/icons/hicolor" 2>/dev/null || true
command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 >/dev/null 2>&1 || true
# Remove the install from before the rename (wavelink / wavelinkd / wavectl).
if [ -e "$HOME/.config/systemd/user/wavelinkd.service" ]; then
  systemctl --user disable --now wavelinkd.service 2>/dev/null || true
  rm -f "$HOME/.config/systemd/user/wavelinkd.service"
fi
rm -f "$bin/wavelinkd" "$bin/wavectl" "$bin/wavelink" "$share/applications/wavelink.desktop"
find "$share/icons/hicolor" -name 'wavelink.*' -delete 2>/dev/null || true
if [ -e "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/wavelink.desktop" ]; then
  mv "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/wavelink.desktop" "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/pipemic.desktop"
  sed -i 's/wavelink/pipemic/g; s/Wave Link/Pipemic/g' "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/pipemic.desktop"
fi
systemctl --user daemon-reload
# pipemicd runs while the Pipemic app does: the app starts it and Quit stops
# it. Login starts the app in the tray ("Open at login"), not the bare daemon.
autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/pipemic.desktop"
if systemctl --user is-enabled pipemicd.service >/dev/null 2>&1; then
  systemctl --user disable pipemicd.service >/dev/null 2>&1 || true
  if [ ! -e "$autostart" ]; then
    mkdir -p "$(dirname "$autostart")"
    printf '[Desktop Entry]\nType=Application\nName=Pipemic\nExec=pipemic --background\nIcon=pipemic\nX-GNOME-Autostart-enabled=true\n' > "$autostart"
  fi
fi
if systemctl --user is-active pipemicd.service >/dev/null 2>&1 || pgrep -x pipemic >/dev/null; then
  systemctl --user restart pipemicd.service
fi
# A window left open would keep running the old build: restart it.
if pgrep -x pipemic >/dev/null; then
  pkill -x pipemic
  for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -x pipemic >/dev/null || break; sleep 0.2; done
  setsid -f "$bin/pipemic" >/dev/null 2>&1 < /dev/null
  echo "restarted the open Pipemic window"
fi
echo "installed to $bin"
