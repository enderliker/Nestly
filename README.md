# nestly

A self-healing, modular Discord bot. A Rust supervisor owns a star of
independently supervised processes; a Node 24 launcher verifies and installs
signed release binaries and streams every log line to the container console.

- **One Discord gateway** (`nestly-edge`). Every module has its own
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
launcher (Node 24)  -> nestly-supervisor -> nestly-edge (single gateway)
                                      -> nestly-mod-music (own REST client)
                                      -> nestly-mod-ops   (own REST client)
```

IPC is a star: the supervisor brokers everything. Children get one inherited
`AF_UNIX` socketpair on file descriptor 3, verified with `SO_PEERCRED`, a
per-spawn 32-byte nonce, postcard frames, and a hard 2 MiB frame cap. Modules
never talk to each other.

## Quick start

Requirements: Node.js 24 LTS, and for building from source Rust stable 1.89+.

```bash
git clone https://github.com/enderliker/nestly
cd nestly
cargo build --release
cp config/nestly.example.toml nestly.toml   # then fill in real ids
export DISCORD_TOKEN=...
export NESTLY_BIN_DIR="$PWD/target/release"
cargo run --release -p nestly-supervisor -- --config nestly.toml
```

Container install (downloads the release, verifies SHA-256, installs under
`/home/container/nestlybins`, starts the supervisor):

```bash
node /home/container/nestly/js/start.mjs
```

Launcher environment:

| Variable | Purpose |
|---|---|
| `DISCORD_TOKEN` | Bot token, required |
| `DISCORD_TOKEN_<MODULE>` | Optional per-module override |
| `NESTLY_REPO` | Allowlisted repository (default `enderliker/nestly`, legacy `RSBOT_REPO` supported) |
| `NESTLY_RELEASE` | `latest` or a release tag (legacy `RSBOT_RELEASE` supported) |
| `NESTLY_PIN_VERSION` | Optional reproducible pin (legacy `RSBOT_PIN_VERSION` supported) |
| `NESTLY_INSTALL_DIR` | Default `/home/container/nestlybins` |
| `NESTLY_STATE_DIR` | Default `/home/container/nestly-state` |
| `NESTLY_CONFIG` | Config path passed to the supervisor |
| `NESTLY_PUBKEY` | Pinned Ed25519 public key (PEM or base64 SPKI) |
| `NESTLY_GITHUB_TOKEN` | Read-only token for private release downloads |
| `NESTLY_SKIP_UPDATE` | Start the verified installed version |
| `NESTLY_RESTART_BUDGET` | Supervisor restarts (default 3) |

With `NESTLY_PUBKEY` unset the launcher still enforces SHA-256 on every asset
and prints a warning that the manifest signature was not checked.

## Commands

| Command | Module | Behaviour |
|---|---|---|
| `/music-play` | music | Queue and play audio from YouTube or SoundCloud |
| `/music-skip` | music | Skip the currently playing track |
| `/music-pause` | music | Pause or resume audio playback |
| `/music-stop` | music | Stop playback and clear the queue |
| `/music-nowplaying` | music | Show current track metadata and duration |
| `/music-queue` | music | View upcoming tracks in the queue |
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
crates/nestly-core          config, manifests, state, redaction
crates/nestly-ipc           framing, handshake, envelopes
crates/nestly-sys           the only crate allowed to use unsafe
crates/nestly-supervisor    lifecycle, registry, health, alerts, log mux
crates/nestly-edge          gateway, command registry, fallback replies
crates/nestly-module-sdk    module runtime used by every module
crates/modules/music        music playback (YouTube, SoundCloud, Songbird)
crates/modules/ops          status and admin restart
js/                         zero-dependency Node 24 launcher
config/                     example configuration
docs/                       architecture, security, modules, operations
scripts/                    boot bench, new module, release tooling
```

## License

Proprietary. All rights reserved.