# Crossbow

<!-- simit:badges:start -->

[![CI](https://img.shields.io/badge/CI-managed-2088ff)](.forgejo/workflows/ci.yaml) [![crates.io](https://img.shields.io/badge/crates.io-ready-f46623)](https://crates.io/crates/crossbow-switch)

<!-- simit:badges:end -->

Crossbow is a Nix flake for QEMU-free cross-compilation. It models compilation and execution separately: a toolchain builds a host artifact, and an executor describes how checks run.

Phase 1 implements Linux-to-Linux package cross-compilation through Zig/LLVM and exposes NixOS helpers for cache-shaped single-switch deployments and strict cross-system proofs. WASM, Windows, and Darwin targets are present in the public platform map, but their backends intentionally fail until their sysroot and executor stories are implemented.

## Public API

- `lib.targets`: canonical target descriptors.
- `lib.nixSystemToZigTarget`: Nix system string to Zig target mapping.
- `lib.nixSystemToGnuConfig`: Nix system string to GNU config mapping.
- `lib.mkCross`: build a package function with an inferred OS-pair toolchain.
- `lib.mkCrossCheck`: create a separate check derivation with a pluggable executor.
- `lib.mkNixosSwitchSystem`: evaluate a cache-shaped host NixOS system for one `nixos-rebuild switch`, while explicit package overrides may be cross-built.
- `lib.mkNixosStrictCrossSystem`: evaluate a full strict-cross NixOS system with explicit `buildPlatform` and `hostPlatform`.
- `lib.mkNixosNativeSubstitutedSystem`: evaluate a host-native NixOS system for cache-first QEMU-free substitution.
- `lib.mkNixosCrossSystem`: compatibility alias for `mkNixosSwitchSystem`.
- `lib.mkCrossOverlay`: build-side cross package override module helper for cache-shaped NixOS systems.
- `lib.hardwareProfiles`: optional target hardware profiles such as `rockpro64`.
- `lib.mkOptimizedHostPlatform`: merge a hardware profile into a Nix host platform descriptor.
- `lib.withCrossSupport`: augment package outputs with cross variants.

## Target Hardware Optimization

Package-level cross or local optimized builds may opt into host hardware tuning without changing the whole NixOS platform:

```nix
crossbow.lib.applyBuildOptimization {
  inherit pkgs package;
  profile = crossbow.lib.buildOptimizationProfiles.fast-local;
  hardwareOptimization = crossbow.lib.hardwareProfiles.rockpro64;
}
```

Strict full-system proof builds are still available separately:

```nix
crossbow.lib.mkNixosStrictCrossSystem {
  build = "x86_64-linux";
  host = "aarch64-linux";
  modules = [ ./root/hosts/thething ];
}
```

Profiles are regular Nix platform metadata. The `rockpro64` profile currently sets:

```nix
{
  gcc.arch = "armv8-a";
  gcc.tune = "cortex-a72.cortex-a53";
}
```

This keeps the ABI at the normal `aarch64-linux` baseline while asking compilers that honor nixpkgs platform metadata to tune code generation for RK3399-class cores.

## Cache Modes

Crossbow names cache strategy explicitly:

- `strict-cross`: the build machine compiles host artifacts. This is the strongest QEMU-free proof, but nixpkgs binary cache hits are usually low because cross derivation paths differ from native host-system paths.
- `cache-shaped-with-cross-overrides`: host-system derivations keep their normal cache shape, while explicit package overrides may use cross derivations from the build machine.
- `native-substituted`: the build machine evaluates or orchestrates a host-native system and downloads host-system paths from substituters. This is the right mode when you want `cache.nixos.org` aarch64 binaries on an x86_64 machine.
- `remote-native`: use substituters first and route missing host-system builds to native hardware.

## Supported Now

- `linux -> linux`, including `x86_64-linux -> aarch64-linux`.
- `native-builder` and `skip` executor descriptors.

## Declared But Stubbed

- `any -> wasm32-wasi`
- `linux/darwin -> windows`
- `linux -> darwin`, which requires a user-provided Apple SDK and osxcross.

Unsupported pairs fail with explicit error messages instead of silently substituting another strategy.
