# Architecture

## Process model

| Process | Responsibility |
|---|---|
| `start.mjs` (Node 24) | Resolve, verify, install release binaries; spawn the supervisor; forward signals; prefix and stream logs; restart the supervisor up to a bounded budget |
| `rsbot-supervisor` | Config, registry, spawn, health, restart policy, IPC broker, log multiplexer, redaction, alerts, status file |
| `rsbot-edge` | The only gateway connection; interaction conversion; guild allowlist; command registration; fallback replies |
| `rsbot-mod-*` | One feature binary. Own handlers, state, and REST client |

The launcher spawns only the supervisor. The supervisor spawns the edge and
every enabled module as separate OS processes. Each child receives the
supervisor IPC socket as file descriptor 3 and an explicitly allowlisted
environment.

## Star IPC

```
supervisor ──socketpair── edge
           ├─socketpair── music
           └─socketpair── ops
```

Modules never address each other. Interactions flow
`edge -> supervisor -> module`, and the module answers Discord directly with
its own REST client and the interaction token. Availability replies flow
`supervisor -> edge -> Discord`.

The supervisor applies backpressure with bounded channels (256 envelopes), a
2 MiB frame cap, postcard encoding, and a handshake nonce. `SO_PEERCRED` must
match the supervisor uid; a mismatch is fatal.

### Handshake

1. Supervisor creates a socketpair and spawns the child with FD 3.
2. Supervisor sends `Challenge { nonce, protocol }`.
3. Child replies `Hello { nonce, manifest }`.
4. Supervisor validates the manifest against configuration (module) or the
   edge contract (edge), then sends `Welcome { session_id, role, limits }`.
5. Child sends heartbeats every second. Three misses make it `Degraded`;
   further silence kills and restarts it.

A protocol-major mismatch, wrong nonce, or manifest mismatch is a fatal
handshake failure: the service becomes `Disabled`, an alert is raised, and it
is never restarted automatically.

## Lifecycle

```
Starting -> Ready -> Degraded -> Ready
                  \-> Restarting(n) -> Ready
                                     \-> Failed (after the budget)
         \-> Disabled (incompatible handshake)
```

- `Ready` means the handshake completed and the process is alive.
- `Degraded` means heartbeats were missed; routing still falls back if the
  state is not routable.
- `Restarting(n)` waits for the backoff slot (`100/250/500 ms` by default).
- `Failed` is terminal until an operator restarts it.
- A successful run of `stable_reset_secs` resets the attempt counter.

An exit code of zero is still an unexpected exit unless shutdown was requested.

## Command interception

The supervisor owns the command-to-module map derived from validated
configuration. When an interaction arrives:

1. The edge converts it to a compact `InteractionPayload`, enforces the guild
   allowlist, and forwards it.
2. The supervisor validates the payload, resolves the owner, and checks state.
3. If the owner is routable, it sends `CommandInvoke` and starts a 3 s ack
   deadline.
4. Otherwise it sends `Fallback { reason }` to the edge, which replies
   ephemerally: "This module is temporarily unavailable... (music)".
5. A module that dies before acking gets the same fallback. A module that
   acked gets its original response edited instead.
6. A module that never answers times out and falls back.

The edge also replies "Unknown command" for names absent from the registry
once the registry has arrived, so no interaction is left hanging.

## Edge responsibilities

- One shard, `GUILDS` intent, interaction and ready event flags.
- Discord-side command registration from the accepted registry, using guild
  commands when `guild_id` is set (instant propagation) and global commands
  otherwise.
- Its own REST client and token, authenticated asynchronously after readiness.
- Reconnect/resume is delegated to twilight; a closed gateway stream exits the
  edge so the supervisor can restart it.

## Compatibility validation

Configuration validation and live-manifest validation both fail closed:

- Module ids match `^[a-z][a-z0-9-]{1,31}$` and are unique.
- Command names match `^[a-z0-9][a-z0-9_-]{0,31}$`, are globally unique, and
  match the configured allowlist exactly.
- Option names, descriptions, counts, duplicates, and required-before-optional
  ordering are validated.
- Protocol majors must match; the edge must claim the exclusive `gateway`
  capability and own no commands.
- Exclusive capabilities are unique across modules.
- Every `requires` capability has a provider (built-ins: `discord.rest`,
  `discord.gateway`, `supervisor.control`, `supervisor.status`).
- Declared RSS must fit both the per-module limit and the total budget.
- Event subscriptions are allowlisted.

## Boot budget

`bot_boot_ms` is supervisor start to every service `Ready`. The boot path has
no network calls: token validation, gateway connect, and command registration
happen after `Ready`. The supervisor spawns all children concurrently and
uses a single-threaded runtime; the edge uses two worker threads.

`rsbot-supervisor --boot-probe` prints:

```json
{"ok":true,"boot_ms":123,"budget_ms":1000,"services":[...]}
```

`scripts/boot-bench.sh` repeats the probe and reports the maximum.

## Deliberate decisions

- Command names are valid Discord names (`music-play`), not dotted names from
  the early plan draft; Discord rejects dots.
- A single 2 MiB frame cap applies to every IPC frame; the plan's separate
  1 MiB control cap was simplified.
- The supervisor runs on a current-thread runtime. It is event-loop bound, and
  this keeps boot and memory lower; the plan's two-worker suggestion is an
  optimization only.
- The launcher installs versioned directories and passes `RSBOT_BIN_DIR`; the
  configuration names services by bare file name and never pins a version.