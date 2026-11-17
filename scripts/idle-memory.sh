#!/usr/bin/env bash
# Measures the tray app's idle memory: the app plus its webview engine's
# helper processes, with the popover hidden.
#
#   scripts/idle-memory.sh [--wait SECONDS] [BINARY]
#   scripts/idle-memory.sh --pid PID
#
# With BINARY (default target/release/wallpaper-rotator), it starts the app
# with a fresh WALLPAPER_ROTATOR_HOME under .verify/, waits (default 60 s),
# measures, then quits the app and deletes the home. The home holds the three
# bundled synthetic favorites, rotation is paused and a current photo is set
# with the CLI's `rotate --dry-run` (wallpaper-rotator-cli next to BINARY), so
# the app neither opens the popover for setup nor changes the desktop.
# With --pid, it measures an app that is already running and leaves it alone.
#
# What it sums, per OS (scripts/idle-memory.ps1 does Windows):
#   macOS  `footprint` (phys_footprint) of the app and the com.apple.WebKit.*
#          XPC processes it uses. Those run under launchd, not as children, so
#          they are matched by responsible process: the app itself when it runs
#          from its .app bundle, or the terminal that launched a bare binary.
#          In the second case, WebKit processes that were already running
#          before the launch are left out.
#   Linux  PSS (smaps_rollup; RSS if unreadable) of the app and every process
#          it started (WebKitGTK's WebKitWebProcess and WebKitNetworkProcess).
#
# Only one copy of the app runs per identifier: a second launch hands over to
# the first and exits. Quit the installed app first, or build the binary under
# another identifier, for example
#   TAURI_CONFIG='{"identifier":"dev.dflippojr.wallpaper-rotator.measure"}' \
#     cargo build --release -p wallpaper-rotator -p rotator-core \
#     --bin wallpaper-rotator --bin wallpaper-rotator-cli --features tauri/custom-protocol
set -euo pipefail

cd "$(dirname "$0")/.."
wait_secs=60
pid=""
binary=""
before_webkit=""
while [ $# -gt 0 ]; do
  case "$1" in
    --wait) wait_secs=$2; shift 2 ;;
    --pid) pid=$2; shift 2 ;;
    -h|--help) sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) binary=$1; shift ;;
  esac
done

os=$(uname -s)
case "$os" in
  Darwin|Linux) ;;
  *) echo "unsupported OS $os; on Windows run scripts/idle-memory.ps1" >&2; exit 1 ;;
esac

home=""
cleanup() {
  if [ -n "$home" ]; then
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      for _ in 1 2 3 4 5 6 7 8 9 10; do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -rf "$home"
  fi
}
trap cleanup EXIT

if [ -z "$pid" ]; then
  binary=${binary:-target/release/wallpaper-rotator}
  cli="$(dirname "$binary")/wallpaper-rotator-cli"
  [ -x "$binary" ] || { echo "no app binary at $binary" >&2; exit 1; }
  [ -x "$cli" ] || { echo "no CLI next to the app at $cli" >&2; exit 1; }
  mkdir -p .verify
  home=$(mktemp -d "$PWD/.verify/idle-memory.XXXXXX")
  mkdir -p "$home/favorites"
  cp app/tests/e2e/fixtures/*.png "$home/favorites/"
  cat >"$home/config.toml" <<'EOF'
sources = ["favorites"]
interval_minutes = 0
paused = true
start_at_login = false
per_display = false
cache_cap_mb = 20
EOF
  # A current photo on disk, so the paused app has nothing to rotate. A dry
  # run never calls the OS wallpaper setter.
  WALLPAPER_ROTATOR_HOME=$home ROTATOR_SIZE=320x200 "$cli" rotate --dry-run >/dev/null 2>&1
  before_webkit=$(pgrep -f 'com\.apple\.WebKit\.' | tr '\n' ' ' || true)
  WALLPAPER_ROTATOR_HOME=$home UNSPLASH_ACCESS_KEY='' PEXELS_API_KEY='' \
    "$binary" >"$home/app.out" 2>&1 &
  pid=$!
  echo "started $binary (pid $pid); measuring in ${wait_secs}s" >&2
  sleep "$wait_secs"
  if ! kill -0 "$pid" 2>/dev/null; then
    echo "the app exited before the measurement; is another instance running?" >&2
    cat "$home/app.out" >&2
    exit 1
  fi
fi
kill -0 "$pid" 2>/dev/null || { echo "no process $pid" >&2; exit 1; }

# Prints "pid name bytes" per measured process.
measure_macos() {
  local helpers
  # The responsible process of a WebKit XPC service is the app that asked
  # for it (libSystem's responsibility_get_pid_responsible_for_pid).
  helpers=$(/usr/bin/python3 - "$pid" "$before_webkit" <<'EOF'
import ctypes, subprocess, sys
app, earlier = int(sys.argv[1]), set(sys.argv[2].split())
responsible = ctypes.CDLL(None).responsibility_get_pid_responsible_for_pid
responsible.restype, responsible.argtypes = ctypes.c_int, [ctypes.c_int]
owner = responsible(app)
ps = subprocess.run(["ps", "-axo", "pid=,comm="], capture_output=True, text=True).stdout
for line in ps.splitlines():
    p, _, comm = line.strip().partition(" ")
    if "com.apple.WebKit." in comm and p not in earlier and responsible(int(p)) == owner:
        print(p)
EOF
  )
  local args=(-p "$pid")
  for h in $helpers; do args+=(-p "$h"); done
  footprint -f bytes --noCategories "${args[@]}" 2>/dev/null |
    sed -n 's/^\(.*\) \[\([0-9]*\)\]:.*Footprint: \([0-9]*\) B.*$/\2 \1 \3/p'
}

measure_linux() {
  local all=" $pid " grew=1 p pp
  while [ $grew = 1 ]; do
    grew=0
    while read -r p pp; do
      case "$all" in *" $pp "*) case "$all" in *" $p "*) ;; *) all="$all$p "; grew=1 ;; esac ;; esac
    done < <(ps -eo pid=,ppid=)
  done
  for p in $all; do
    local kb name
    kb=$(awk '/^Pss:/ {print $2}' "/proc/$p/smaps_rollup" 2>/dev/null || true)
    [ -n "$kb" ] || kb=$(awk '/^VmRSS:/ {print $2}' "/proc/$p/status" 2>/dev/null || true)
    name=$(cat "/proc/$p/comm" 2>/dev/null || echo "?")
    [ -n "$kb" ] && echo "$p $name $((kb * 1024))"
  done
}

if [ "$os" = Darwin ]; then rows=$(measure_macos); else rows=$(measure_linux); fi
[ -n "$rows" ] || { echo "could not measure process $pid" >&2; exit 1; }

metric=$([ "$os" = Darwin ] && echo "footprint" || echo "PSS")
echo "| Process | PID | ${metric} (MB) |"
echo "| --- | --- | --- |"
echo "$rows" | awk '
  { name = $2; for (i = 3; i < NF; i++) name = name " " $i
    mb = $NF / 1048576; total += mb
    printf "| `%s` | %s | %.1f |\n", name, $1, mb }
  END { printf "| **Total** | | **%.1f** |\n", total }'
