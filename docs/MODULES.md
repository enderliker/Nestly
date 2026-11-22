# Writing a module

A module is one Rust binary that links `rsbot-module-sdk`. The supervisor
spawns it with an inherited IPC socket and a Discord token, and the SDK handles
the handshake, heartbeats, dispatch, replies, and shutdown.

## Minimal module

```rust
use std::process::ExitCode;

use rsbot_core::{CommandSpec, ModuleLimits, ModuleManifest, ProtocolVersion};
use rsbot_module_sdk::{ModuleBuilder, Response};

fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: "ping".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: ProtocolVersion::default(),
        commands: vec![CommandSpec::new("ping", "Reply with pong")],
        events: vec!["interaction_create".to_owned()],
        provides: Vec::new(),
        requires: vec!["discord.rest".to_owned()],
        exclusive: Vec::new(),
        limits: ModuleLimits::default(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match ModuleBuilder::new(manifest())
        .command("ping", |_context| async move {
            Ok(Response::ephemeral("pong"))
        })
        .run()
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            eprintln!("rsbot-mod-ping failed: {source}");
            ExitCode::FAILURE
        }
    }
}
```

`scripts/new-module.sh <name>` scaffolds the crate, manifest, test, and
workspace entry.

## Manifest rules

| Field | Rule |
|---|---|
| `id` | `^[a-z][a-z0-9-]{1,31}$`, unique |
| `version` | Semantic version |
| `protocol` | Major must match the supervisor (currently 1) |
| `commands[].name` | `^[a-z0-9][a-z0-9_-]{0,31}$`, globally unique |
| `commands[].description` | At most 100 characters; a default is derived if empty |
| `commands[].options` | At most 25; names `^[a-z0-9_-]{1,32}$`; required options before optional ones |
| `events` | Subset of `interaction_create`, `voice_state_update`, `guild_member_add`, `guild_member_remove` |
| `provides` / `requires` | Capability strings; every requirement needs one provider |
| `exclusive` | Capability may be claimed by at most one module |
| `limits.max_rss_mb` | 1–4096 and within the total budget |
| `limits.max_ipc_msgs_per_sec` | 1–100000 |
| `limits.max_log_lines_per_sec` | 1–10000 |

The configuration file must declare the same module id, commands, events, and
capabilities. A mismatch disables the module with an alert; it is never
silently accepted.

## Command options

```rust
CommandSpec::new("ops-restart", "Restart a supervised module").with_options(vec![
    CommandOptionSpec::new("module", "Module id", true, CommandOptionKind::String),
])
```

Read an option in a handler:

```rust
let module = context.option_string("module").unwrap_or_default();
```

The edge registers these options with Discord from the accepted live
manifest; the configuration file only lists command names.

## Replying

| Response | Effect |
|---|---|
| `Response::message(content)` | Public channel message |
| `Response::ephemeral(content)` | Ephemeral message |
| `Response::defer_ephemeral()` | Acknowledge, then edit later |
| `Response::update(content)` | Update the clicked component message |
| `Response::None` | No reply; the supervisor will time the interaction out |

Always answer user-facing errors with a response. Returning `Err` from a
handler produces an internal result but no Discord message.

## Supervisor control

```rust
let status = context.supervisor_status().await?;      // JSON snapshot
let reply = context.restart_module("music").await?;   // admin-gated by the module
```

Control requests time out after two seconds. The supervisor is the only broker;
modules cannot address each other.

## Testing

Unit-test manifests, authorization, formatting, and argument parsing without a
Discord connection. Integration tests in the supervisor spawn real module
binaries behind a fake edge and assert the boot probe. A module that panics
exits; the supervisor restarts it and raises an alert with the redacted stderr
ring.

## Security checklist

- Never log a token, interaction token, or `Authorization` header.
- Treat all option values as untrusted; bound their length.
- Re-check authorization for destructive operations.
- Keep `stdin` unused and write logs to stdout/stderr only.
- Declare the smallest capability set and the real RSS limit.