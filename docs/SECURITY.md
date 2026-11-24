# Security model

The design assumes one trusted operator, untrusted Discord input, and modules
that may be buggy or compromised. No secret may reach a log, an alert, a status
file, argv, a core dump, or another module.

## Threats and controls

| Threat | Control |
|---|---|
| Token leaked through logs or alerts | Env-only secrets; exact-value Aho-Corasick redaction plus Discord-token, `Authorization`, and webhook patterns applied to child logs, alert reasons, and alert bodies |
| Token leaked through argv | Tokens are never passed as arguments; children get them through the environment only |
| Token leaked through a crash | `RLIMIT_CORE=0`, `PR_SET_DUMPABLE=0`, no token in panic output |
| IPC impersonation | Inherited socketpair, `SO_PEERCRED` uid check, per-spawn 32-byte nonce, handshake validation |
| Malformed or oversized IPC | 2 MiB hard frame cap, postcard decode, bounded queues, handshake timeout |
| Malicious module | Explicit env allowlist, cleared environment, per-module state directory, no module-to-module channel, declared capabilities, rate limits |
| Duplicate or conflicting modules | Startup rejection of duplicate commands and exclusive capabilities, protocol mismatch, missing capability, memory budget |
| Guild abuse | `allowed_guild_ids` enforced in the supervisor and the edge; direct messages are rejected |
| Admin command abuse | `/ops-restart` requires a user in `discord.admin_user_ids`; the module re-checks server-side |
| Oversized Discord input | Interaction token, custom id, command name, and data length validated before routing; JSON capped at 512 KiB |
| Supply chain | SHA-256 verification of every asset; Ed25519 manifest signature when a pinned key is configured; HTTPS-only downloads; host allowlist across redirects; repository allowlist; size caps; atomic install with mode 0700/0755 |
| Path traversal in a release | Asset names must be flat `nestly-*` names; install paths are resolved and prefix-checked; version strings are validated |
| Concurrent launchers | Atomic lock file with live-pid detection and stale-lock recovery |
| Log flooding | 8 KiB line cap, per-module ring buffer, bounded IPC queues |

## Process hardening

Every child is configured with:

- `PR_SET_PDEATHSIG(SIGTERM)` so children die with the supervisor.
- `PR_SET_NO_NEW_PRIVS`.
- `PR_SET_DUMPABLE=0`.
- `RLIMIT_CORE=0`.
- `RLIMIT_NOFILE=1024`.
- `umask(0o077)`.
- `env_clear()` followed by an explicit allowlist: `PATH`, `RUST_LOG`, `TZ`,
  the module token variable, Discord application/guild ids, the admin
  allowlist, the module id, and its private state directory.
- `stdin` from `/dev/null`, `stdout`/`stderr` piped to the supervisor.
- Rejection of symlinked or non-executable binaries.

The only crate allowed to contain `unsafe` is `nestly-sys`; every block is a
small audited libc wrapper.

## Secrets

- `DISCORD_TOKEN` is required; `DISCORD_TOKEN_<MODULE>` optionally overrides a
  module token.
- `NESTLY_OPS_TOKEN` (or legacy `RSBOT_OPS_TOKEN`) optionally supplies a dedicated alert token.
- `NESTLY_ALERT_CHANNEL` (or legacy `RSBOT_ALERT_CHANNEL`) optionally supplies the alert channel.
- Tokens are redacted by exact value before reaching any sink.
- The launcher keeps `NESTLY_GITHUB_TOKEN` to itself; it is not part of the
  child environment.

## Release verification

1. The repository must be allowlisted; release tags must match a semver shape.
2. The manifest is downloaded over HTTPS from an allowlisted host, following
   at most five redirects that are re-validated at every hop.
3. When `NESTLY_PUBKEY` (or `RSBOT_PUBKEY`) is set, the detached Ed25519 signature over the exact
   manifest bytes is verified; a bad signature aborts the install.
4. Manifest schema, version, target, asset names, sizes, and SHA-256 digests
   are validated before any binary is downloaded.
5. Assets are hashed while streaming and compared in constant time.
6. Installation writes into a private temporary directory, fsyncs, renames
   into place, and updates the `current` pointer by rename (never a symlink).
7. A corrupt install is replaced; a failed update falls back to a verified
   installed version; an unverified binary is never executed.

## Reporting

This is a private repository. Report vulnerabilities to the repository owner
through a private security advisory; do not open a public issue.