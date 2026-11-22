# rsbot

A self-healing, modular Discord bot. A Rust supervisor owns a star of
independently supervised processes; a Node 24 launcher verifies and installs
signed release binaries and streams every log line to the container console.

- **One Discord gateway** (`rsbot-edge`). Every module has its own
  authenticated `twilight-http` client.
- **Command interception.** When a module is down, the user gets an immediate
  "temporarily unavailable" reply instead of silence.
- **Bounded recovery.** Up to three consecutive restarts with backoff, then a
  redacted Discord alert with the module's recent logs.
- **Strict compatibility.** Duplicate commands, conflicting exclusive
  capabilities, protocol-major mismatches, missing capabilities, and memory
  budget violations are rejected before Discord sees them.
- **Sub-second boot.** Readiness is local-only: no DNS, gateway, or Discord
  REST in the boot path.

## Architecture

```
launcher (Node 24)  -> rsbot-supervisor -> rsbot-edge (single gateway)
                                      -> rsbot-mod-music (own REST client)
                                      -> rsbot-mod-ops   (own REST client)
```

IPC is a star: the supervisor brokers everything. Children get one inherited
`AF_UNIX` socketpair on file descriptor 3, verified with `SO_PEERCRED`, a
per-spawn 32-byte nonce, postcard frames, and a hard 2 MiB frame cap. Modules
never talk to each other.

## Quick start

Requirements: Node.js 24 LTS, and for building from source Rust stable 1.89+.

```bash
git clone https://github.com/enderliker/rsbot
cd rsbot
cargo build --release
cp config/rsbot.example.toml rsbot.toml   # then fill in real ids
export DISCORD_TOKEN=...
export RSBOT_BIN_DIR="$PWD/target/release"
cargo run --release -p rsbot-supervisor -- --config rsbot.toml
```

Container install (downloads the release, verifies SHA-256, installs under
`/home/container/rsbotbins`, starts the supervisor):

```bash
node /home/container/rsbot/js/start.mjs
```

Launcher environment:

| Variable | Purpose |
|---|---|
| `DISCORD_TOKEN` | Bot token, required |
| `DISCORD_TOKEN_<MODULE>` | Optional per-module override |
| `RSBOT_REPO` | Allowlisted repository (default `enderliker/rsbot`) |
| `RSBOT_RELEASE` | `latest` or a release tag |
| `RSBOT_PIN_VERSION` | Optional reproducible pin |
| `RSBOT_INSTALL_DIR` | Default `/home/container/rsbotbins` |
| `RSBOT_STATE_DIR` | Default `/home/container/rsbot-state` |
| `RSBOT_CONFIG` | Config path passed to the supervisor |
| `RSBOT_PUBKEY` | Pinned Ed25519 public key (PEM or base64 SPKI) |
| `RSBOT_GITHUB_TOKEN` | Read-only token for private release downloads |
| `RSBOT_SKIP_UPDATE` | Start the verified installed version |
| `RSBOT_RESTART_BUDGET` | Supervisor restarts (default 3) |

With `RSBOT_PUBKEY` unset the launcher still enforces SHA-256 on every asset
and prints a warning that the manifest signature was not checked.

## Commands

| Command | Module | Behaviour |
|---|---|---|
| `/music-play` | music | Lifecycle stub reply |
| `/music-skip` | music | Lifecycle stub reply |
| `/ops-status` | ops | Uptime, gateway readiness, service states |
| `/ops-modules` | ops | Per-service state list |
| `/ops-restart module:<id>` | ops | Admin-only restart (allowlisted users) |

Command names are valid Discord names (`music-play`, not `music.play`).

## Testing

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cd js && node --test test/*.test.mjs
```

The suite covers framing and handshake limits, secret redaction, the restart
state machine, fallback interception, incompatible manifests, duplicate
commands, two children logging 10,000 lines each without loss, launcher
signature and checksum verification, path traversal, atomic installs,
concurrent-launcher locking, signal forwarding, and a boot probe with the real
module binaries under 1,000 ms.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Security model](docs/SECURITY.md)
- [Writing a module](docs/MODULES.md)
- [Operations](docs/OPERATIONS.md)

## Repository layout

```
crates/rsbot-core          config, manifests, state, redaction
crates/rsbot-ipc           framing, handshake, envelopes
crates/rsbot-sys           the only crate allowed to use unsafe
crates/rsbot-supervisor    lifecycle, registry, health, alerts, log mux
crates/rsbot-edge          gateway, command registry, fallback replies
crates/rsbot-module-sdk    module runtime used by every module
crates/modules/music       lifecycle stub
crates/modules/ops         status and admin restart
js/                        zero-dependency Node 24 launcher
config/                    example configuration
docs/                      architecture, security, modules, operations
scripts/                   boot bench, new module, release tooling
```

## License

Proprietary. All rights reserved.