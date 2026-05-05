# Crossbow

Crossbow is a Nix flake for QEMU-free cross-compilation. It models compilation and execution separately: a toolchain builds a host artifact, and an executor describes how checks run.

Phase 1 implements Linux-to-Linux package cross-compilation through Zig/LLVM and exposes the strict NixOS cross-system helper needed for Canix's atlas-to-thething proof. WASM, Windows, and Darwin targets are present in the public platform map, but their backends intentionally fail until their sysroot and executor stories are implemented.

## Public API

- `lib.targets`: canonical target descriptors.
- `lib.nixSystemToZigTarget`: Nix system string to Zig target mapping.
- `lib.nixSystemToGnuConfig`: Nix system string to GNU config mapping.
- `lib.mkCross`: build a package function with an inferred OS-pair toolchain.
- `lib.mkCrossCheck`: create a separate check derivation with a pluggable executor.
- `lib.mkNixosCrossSystem`: evaluate a NixOS system with explicit `buildPlatform` and `hostPlatform`.
- `lib.withCrossSupport`: augment package outputs with cross variants.

## Supported Now

- `linux -> linux`, including `x86_64-linux -> aarch64-linux`.
- `native-builder` and `skip` executor descriptors.

## Declared But Stubbed

- `any -> wasm32-wasi`
- `linux/darwin -> windows`
- `linux -> darwin`, which requires a user-provided Apple SDK and osxcross.

Unsupported pairs fail with explicit error messages instead of silently substituting another strategy.
