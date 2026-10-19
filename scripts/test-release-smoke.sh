#!/usr/bin/env bash
# Regression checks without installing software or launching a desktop app.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
failures=0

# Exercise the actual log inspector with no rotator.log, as on a fresh startup.
sed -n '/^inspect_log() {$/,/^}$/p' "$root/scripts/smoke-installed.sh" > "$scratch/inspect.sh"
check_log() {
  local output=$1 expected=$2 result=0
  printf '%s\n' "$output" > "$scratch/test-output.log"
  bash -c 'set -euo pipefail; source "$1"; evidence=$2; kind=test; log="$2/absent.log"; inspect_log' \
    _ "$scratch/inspect.sh" "$scratch" > "$scratch/result.txt" 2>&1 || result=$?
  if [ "$result" = "$expected" ]; then
    printf 'PASS: launch output %s (exit %s)\n' "$output" "$result"
  else
    printf 'FAIL: launch output %s: expected exit %s, got %s\n' "$output" "$expected" "$result"
    cat "$scratch/result.txt"
    failures=$((failures + 1))
  fi
}
check_log 'startup ready' 0
check_log 'failed to initialize tray' 1
check_log 'Gtk-ERROR: initialization failed' 1

# Mock only the compiler tools; run the real preparer in an isolated tree.
mkdir -p "$scratch/project/scripts" "$scratch/bin"
cp "$root/scripts/prepare-bundle-cli.sh" "$scratch/project/scripts/"
cat > "$scratch/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
while [ "$1" != --target ]; do shift; done
triple=$2
mkdir -p "target/$triple/release"
printf '%s\n' "$triple" > "target/$triple/release/wallpaper-rotator-cli"
MOCK
cat > "$scratch/bin/lipo" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
while [ "$1" != -output ]; do shift; done
printf 'universal\n' > "$2"
MOCK
chmod +x "$scratch/bin/cargo" "$scratch/bin/lipo"
PATH="$scratch/bin:$PATH" CARGO_TARGET_DIR=target bash "$scratch/project/scripts/prepare-bundle-cli.sh" universal-apple-darwin
for triple in aarch64-apple-darwin x86_64-apple-darwin universal-apple-darwin; do
  if [ -s "$scratch/project/app/binaries/wallpaper-rotator-cli-$triple" ]; then
    printf 'PASS: prepared %s sidecar\n' "$triple"
  else
    printf 'FAIL: missing %s sidecar needed by Tauri\n' "$triple"
    failures=$((failures + 1))
  fi
done
[ "$failures" = 0 ]
