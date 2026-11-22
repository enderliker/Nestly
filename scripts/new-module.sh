#!/usr/bin/env bash
# Scaffolds a module crate, manifest, unit test, and workspace entry.
set -euo pipefail

name="${1:-}"
if [[ ! "$name" =~ ^[a-z][a-z0-9-]{0,31}$ ]]; then
  printf 'usage: %s <module-name> (lowercase letters, digits, dashes)\n' "$0" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate_name="rsbot-mod-$name"
dir="$root/crates/modules/$name"

if [[ -e "$dir" ]]; then
  printf 'module directory already exists: %s\n' "$dir" >&2
  exit 1
fi

mkdir -p "$dir/src"

cat > "$dir/Cargo.toml" <<EOF
[package]
name = "$crate_name"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true

[[bin]]
name = "$crate_name"
path = "src/main.rs"

[dependencies]
rsbot-core = { path = "../../rsbot-core" }
rsbot-module-sdk = { path = "../../rsbot-module-sdk" }
serde_json.workspace = true
tokio.workspace = true

[lints]
workspace = true
EOF

cat > "$dir/src/main.rs" <<EOF
#![forbid(unsafe_code)]
//! $name module.

use std::process::ExitCode;

use rsbot_core::{CommandSpec, ModuleLimits, ModuleManifest, ProtocolVersion};
use rsbot_module_sdk::{ModuleBuilder, Response};

/// Commands are valid Discord names (no dots).
#[must_use]
pub fn manifest() -> ModuleManifest {
    ModuleManifest {
        id: "$name".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol: ProtocolVersion::default(),
        commands: vec![CommandSpec::new("$name", "$name command")],
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
        .command("$name", |_context| async move {
            Ok(Response::ephemeral("$name is alive."))
        })
        .run()
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(source) => {
            eprintln!("$crate_name failed: {source}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::manifest;

    #[test]
    fn manifest_is_valid() {
        manifest().validate().unwrap();
        assert_eq!(manifest().id, "$name");
    }
}
EOF

python3 - "$root/Cargo.toml" "$name" <<'PY'
import sys
from pathlib import Path

workspace = Path(sys.argv[1])
name = sys.argv[2]
text = workspace.read_text()
entry = f'    "crates/modules/{name}",\n'
if entry in text:
    raise SystemExit('workspace entry already exists')
marker = '    "crates/modules/ops",\n'
if marker not in text:
    raise SystemExit('cannot find the modules list in Cargo.toml')
workspace.write_text(text.replace(marker, marker + entry))
PY

printf 'created crates/modules/%s\n' "$name"
printf 'Add it to your configuration with commands = ["%s"].\n' "$name"