#!/usr/bin/env bash
# Runs a command against a headless GNOME Shell with one 1920x1080 virtual
# monitor, on a private D-Bus session, so the OS wallpaper tests have a real
# GNOME desktop to set, read back and screenshot:
#   scripts/os-wallpaper-gnome.sh cargo test -p rotator-core --test os_wallpaper
# Needs gnome-shell, gsettings-desktop-schemas and the dconf GSettings backend
# (see the os-wallpaper job in .github/workflows/ci.yml), and a system bus
# with logind, as on the ubuntu-24.04 runners.
set -euo pipefail

if [ -z "${ROTATOR_GNOME_SESSION:-}" ]; then
  exec env ROTATOR_GNOME_SESSION=1 dbus-run-session -- "$0" "$@"
fi

log=$(mktemp)
# --unsafe-mode (a hidden flag in GNOME 46) lets the test call
# org.gnome.Shell.Screenshot and this script call org.gnome.Shell.Eval.
# Without it the shell only answers its own allow-listed apps.
gnome-shell --headless --wayland --no-x11 --unsafe-mode \
  --virtual-monitor 1920x1080 > "$log" 2>&1 &
shell=$!
stop() {
  kill "$shell" 2> /dev/null || true
  wait "$shell" 2> /dev/null || true
}
trap stop EXIT

shell_eval() {
  gdbus call --session --dest org.gnome.Shell --object-path /org/gnome/Shell \
    --method org.gnome.Shell.Eval "$1"
}

ready=
for _ in $(seq 60); do
  if ! kill -0 "$shell" 2> /dev/null; then
    echo "gnome-shell exited before it was ready:" >&2
    cat "$log" >&2
    exit 1
  fi
  if shell_eval '!Main.layoutManager._startingUp' 2> /dev/null | grep -q "(true, 'true')"; then
    ready=1
    break
  fi
  sleep 1
done
if [ -z "$ready" ]; then
  echo "gnome-shell did not finish starting in 60 s:" >&2
  cat "$log" >&2
  exit 1
fi

# GNOME starts in the overview, which covers the wallpaper.
shell_eval 'Main.overview.hide()' > /dev/null
echo "$(gnome-shell --version) is up headless; overview hidden"

status=0
"$@" || status=$?
if [ "$status" -ne 0 ]; then
  echo "--- gnome-shell log ---"
  cat "$log"
fi
exit "$status"
