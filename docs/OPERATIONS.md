# Operations

## Container layout

```
/home/container/nestly/js/                 launcher (Node 24)
/home/container/nestly.toml                configuration
/home/container/nestlybins/<version>/      verified release binaries
/home/container/nestlybins/current         version pointer (regular file)
/home/container/nestly-state/              launcher state, lock, logs
```

Start command:

```bash
node /home/container/nestly/js/start.mjs
```

The launcher takes an exclusive lock, reuses a verified install when possible,
otherwise downloads and verifies the release, then starts the supervisor. The
supervisor's stdout and stderr are prefixed `[supervisor] `; module lines
already carry their module field.

## Required environment

| Variable | Required | Notes |
|---|---|---|
| `DISCORD_TOKEN` | yes | Bot token |
| `NESTLY_CONFIG` | recommended | Absolute path to the TOML (legacy `RSBOT_CONFIG` supported) |
| `NESTLY_ALERT_CHANNEL` | optional | Overrides `notifications.channel_id` (legacy `RSBOT_ALERT_CHANNEL` supported) |
| `NESTLY_OPS_TOKEN` | optional | Dedicated alert token (legacy `RSBOT_OPS_TOKEN` supported) |
| `NESTLY_PUBKEY` | recommended | Pinned Ed25519 public key (legacy `RSBOT_PUBKEY` supported) |
| `NESTLY_GITHUB_TOKEN` | private repo | Read-only release access |

## Deploy and upgrade

1. Publish a `v*` tag. Releases contain the manifest, signature, checksums, and
   target-suffixed binaries.
2. Restart the container. The launcher resolves `latest` (or `NESTLY_RELEASE`),
   verifies, installs a new version directory, and switches `current`.
3. A failed download keeps the previous verified version running with a WARN.
4. Rollback: set `NESTLY_RELEASE` to the previous tag, or run with
   `NESTLY_SKIP_UPDATE=1` while the previous version is still installed. Only
   the current and previous versions are retained.

## Alerting

After three consecutive failed restarts the module becomes `Failed` and one
alert per episode is delivered to the notification channel (at most one per
module per `cooldown_secs`). The embed contains module, version, episode,
attempts, exit status, uptime, host, reason, and up to 40 redacted log lines.
`allowed_mentions` is always empty.

If Discord alerting is not configured, the same JSON alert is appended to
`state/alerts/alerts.jsonl`. Delivery failures never block or crash the
supervisor.

## Status

`state/status.json` is rewritten at most once per second:

```json
{
  "uptime_ms": 12345,
  "boot_logged": true,
  "edge_ready": true,
  "services": [
    {"id": "edge", "role": "edge", "state": "ready", "attempts": 0, "pid": 10}
  ]
}
```

`/ops-status` and `/ops-modules` expose the same snapshot through the ops
module. `/ops-restart module:<id>` requires an allowlisted user and is refused
for disabled modules.

## Boot performance

```bash
scripts/boot-bench.sh 50
```

The script builds release binaries, points `NESTLY_BIN_DIR` at them, and runs
`nestly-supervisor --boot-probe` repeatedly. It fails if any run exceeds
1,000 ms. Network access is not part of readiness.

## Failure handling

| Symptom | Response |
|---|---|
| Module exits | Supervisor restarts with backoff; fallback replies meanwhile |
| Module hangs | Three missed heartbeats, SIGTERM, then SIGKILL, restart |
| Incompatible manifest | `Disabled`, one alert, no restart loop |
| Edge exits | Restarted; modules stay up but no gateway events flow |
| Supervisor exits | Launcher restarts it within its budget, then exits non-zero |
| Launcher cannot verify | Refuses to execute anything unverified |

## Troubleshooting

- `launcher: ... failed verification` — the installed files were modified or
  truncated. Remove the version directory and restart.
- `another launcher is already running` — a stale lock is cleared
  automatically; a live pid blocks the second launcher by design.
- `configuration ok` but boot fails — check alert files for manifest mismatch
  reasons; the supervisor reports the exact field.
- `gateway receive error` — network or Discord outage; twilight reconnects. The
  edge does not reconnect manually.
- No command replies — confirm `guild_id` and the allowlist, and that command
  registration succeeded in the edge logs.