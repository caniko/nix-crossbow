# Crossbow vision

Crossbow is a Nix flake for QEMU-free cross-compilation. It models compilation
and execution separately: a toolchain builds a host artifact, and an executor
describes how checks run. Phase 1 implements Linux-to-Linux package
cross-compilation through Zig/LLVM and exposes NixOS helpers for cache-shaped
single-switch deployments and strict cross-system proofs. The `crossbow-switch`
CLI drives the deployment side, and the library is consumed as a flake helper
by downstream infrastructure.

## Problem

Cross-building NixOS normally means either emulating the target with QEMU or
binfmt, or maintaining a parallel native build path. Binary caches are keyed by
system, so a cross-built closure is only useful if it matches what a cache can
substitute — otherwise the deployment still builds everything locally. The
result is slow, hard to verify, and easy to break when a package quietly falls
back to emulation.

## Principles

- **Compilation and execution are separate concerns.** `mkCross` builds with an
  inferred OS-pair toolchain; `mkCrossCheck` runs checks through a pluggable
  executor instead of assuming the target can execute on the build host.
- **Unimplemented platforms fail on purpose.** WASM, Windows, and Darwin targets
  appear in the public platform map, but their backends fail until a sysroot and
  executor story exists. A loud failure beats a silent native build.
- **Plans are authoritative.** Cache-shaped deployments plan the complete
  closure, probe the full substituter union, and retain planned dependency
  outputs, so what gets substituted matches what was planned.
- **Optimization is opt-in per package.** `applyBuildOptimization` and hardware
  profiles such as `rockpro64` tune an individual build without changing the
  whole NixOS platform.
- **Strict proof stays available.** `mkNixosStrictCrossSystem` evaluates a full
  strict-cross system alongside the cache-shaped switch helper.

## Scope

- `lib.targets`, platform maps, and Nix system to Zig/GNU target mappings
- `mkCross`, `mkCrossCheck`, and the `mkNixos*System` evaluation helpers
- `lib.mkCrossOverlay`, `lib.withCrossSupport`, and `lib.mkPinManifest`
- hardware profiles and build optimization profiles
- the `crossbow-switch` CLI for closure planning and switching

## Non-goals

- QEMU or binfmt execution of target binaries as the normal path
- claiming WASM, Windows, or Darwin support before their sysroots and
  executors exist

## Direction

The README states Phase 1 is Linux-to-Linux. Recent work fills in the
cache-shaped story — structured closure planning, runtime route hints with
parallel cache probing, authoritative closure plans, retaining cached runtime
closures across substituters, and a read-only pin manifest API and CLI.

## Place in the fleet

Canix consumes Crossbow as a flake input and depends on the `crossbow-switch`
crate from its operations crate, where it drives cache-shaped NixOS switch
planning for hosts.
