#!/usr/bin/env bash
# Runs the boot probe against the real release binaries and fails above 1 s.
set -euo pipefail

iterations="${1:-5}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
profile_dir="${RSBOT_PROFILE_DIR:-$root/target/release}"

cd "$root"
cargo build --release --workspace --quiet

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

sed "s|/home/container/rsbot-state|$work/state|g" \
  config/rsbot.example.toml > "$work/rsbot.toml"

export RSBOT_BIN_DIR="$profile_dir"
export DISCORD_TOKEN="${DISCORD_TOKEN:-boot-probe-token-0123456789}"

max=0
for run in $(seq 1 "$iterations"); do
  output="$("$profile_dir/rsbot-supervisor" --config "$work/rsbot.toml" --boot-probe)"
  report="$(printf '%s\n' "$output" | grep '^BOOT_PROBE ' | sed 's/^BOOT_PROBE //')"
  if [[ -z "$report" ]]; then
    printf 'run %s produced no boot report:\n%s\n' "$run" "$output" >&2
    exit 1
  fi
  boot_ms="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["boot_ms"])' "$report")"
  ok="$(python3 -c 'import json,sys; print(str(json.loads(sys.argv[1])["ok"]).lower())' "$report")"
  printf 'run %s: ok=%s boot_ms=%s\n' "$run" "$ok" "$boot_ms"
  if [[ "$ok" != "true" ]]; then
    exit 1
  fi
  if (( boot_ms > max )); then
    max="$boot_ms"
  fi
  if (( boot_ms >= 1000 )); then
    printf 'boot budget exceeded: %s ms\n' "$boot_ms" >&2
    exit 1
  fi
done

printf 'max boot_ms=%s over %s runs\n' "$max" "$iterations"