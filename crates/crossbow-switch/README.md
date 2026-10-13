# crossbow-switch

Runtime support for Crossbow's cache-shaped NixOS orchestration, available as
a Rust library and the `crossbow-switch` executable.

The library plans exact derivation and runtime closures, restores cached
runtime paths dependency-first across declared substituters, persists
prepared-state roots, and activates an already prepared toplevel. Missing or
indeterminate prerequisites fail closed. Activation uses substitution-only
policy; remote native realization requires an explicit caller-supplied policy.

```rust
use crossbow_switch::{RealizationPolicy, realization_nix_flags};

let flags = realization_nix_flags(&RealizationPolicy::SubstituteOnly)?;
assert!(flags.windows(2).any(|pair| pair == ["--builders", ""]));
# Ok::<(), crossbow_switch::Error>(())
```

Nix and any selected transport executables must be available at runtime for
operations that invoke them. Pure planning and persisted-state types can be
used independently. See the [repository guide](https://github.com/caniko/nix-crossbow)
for the flake-level compilation API, authoritative plans, and executor policy.

Existing schema-2 plans retain their canonical identifiers and execution
semantics. Use `supports_plan_schema` when loading persisted plans; schema 3
also records runtime paths separately from the derivation build frontier.

## Development

```sh
cargo test --locked --workspace --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
treefmt --ci
```

Licensed under MIT OR Apache-2.0. Both license texts are included in `LICENSE`.
