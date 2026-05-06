# Cache-shaped switch: split the closure along the stdenv/stdenvNoCC seam.
#
#   stdenv      (with CC) → host platform → kernel, glibc, systemd, python …
#                                           keep the native cache shape so
#                                           cache.nixos.org / Attic substitute.
#
#   stdenvNoCC  (text/symlink scaffolding) → build platform → runCommand,
#                                           writeText, writeShellScript,
#                                           symlinkJoin, linkFarm … all of
#                                           NixOS's generated config and
#                                           system.build.toplevel are produced
#                                           by these helpers (see nixpkgs
#                                           build-support/trivial-builders).
#
# Result: the runtime closure is host-native cache-shaped, and every flake-
# specific assembly derivation has system = build, so the build machine can
# realise it with no remote builder, no QEMU, no binfmt.
#
# Caller passes an already-realised build-platform pkgs (e.g.
# `nixpkgs.legacyPackages.${build}` from flake-parts' withSystem). We never
# call `import nixpkgs { system = build; }` ourselves — that would double the
# evaluation cost of every switch. Crossbow is a cheap optimisation path:
# extra work versus a normal native NixOS eval is a handful of attribute
# rebinds in trivial-builders.
{lib}: let
  # Set of trivial-builder attributes that NixOS modules use to assemble
  # text/symlink scaffolding (etc, units, activation scripts, registry JSON,
  # Home Manager wrappers, system.build.toplevel, …). Listed once and reused.
  buildAssemblyAttrNames = {
    runCommand = null;
    runCommandLocal = null;
    runCommandWith = null;
    writeText = null;
    writeTextFile = null;
    writeTextDir = null;
    writeScript = null;
    writeScriptBin = null;
    writeShellScript = null;
    writeShellScriptBin = null;
    writeShellApplication = null;
    writeNginxConfig = null;
    writeCueValidator = null;
    symlinkJoin = null;
    linkFarm = null;
    linkFarmFromDrvs = null;
    concatText = null;
    concatTextFile = null;
    applyPatches = null;
    substituteAll = null;
    substitute = null;
    replaceVars = null;
    replaceVarsWith = null;
    writeReferencesToFile = null;
    # `pkgs.formats.{json,ini,keyValue,toml,yaml,...}` is widely used by
    # NixOS modules (e.g. services.openssh's `sshd.conf-settings` via
    # `formats.keyValue`). Each `.generate` is a closure over the
    # *original* hostPkgs `writeText` captured at pkgs construction
    # time, so shadowing only `writeText` does not reach into it.
    # Replacing the entire `formats` namespace with `buildPkgs.formats`
    # makes generated config files emit `system = build`.
    formats = null;
  };

  # The crucial design choice: we shadow these attrs on `_module.args.pkgs`
  # via a plain `//` rather than via `nixpkgs.overlays`.
  #
  # An overlay applies at the nixpkgs fix-point and is observed by every
  # `callPackage` in pkgs, including the stdenv bootstrap chain — so even
  # overriding just `runCommand` propagates into `stdenv-linux`, glibc, gcc,
  # tzdata, and every aarch64 cache-shape derivation, blowing the binary
  # cache. (Empirically verified: empty overlay leaves stdenv-linux hash
  # untouched; `{ runCommand = buildPkgs.runCommand; }` overlay changes it.)
  #
  # A `_module.args.pkgs` override is *post-fixpoint*: callers in NixOS
  # modules see the swapped helpers, but every package in the runtime
  # closure (kernel, systemd, python, tzdata) was constructed before the
  # shadow and keeps its native cache-shape hash. Substitution from
  # cache.nixos.org and Attic continues to work; only flake-specific
  # scaffolding produced via the swapped helpers gets `system = build`.
  mkBuildAssemblyPkgsModule = {
    nixpkgs,
    host,
    buildPkgs,
  }: {
    config,
    lib,
    ...
  }: {
    # Construct host pkgs ourselves from `nixpkgs` (NixOS would otherwise do
    # the same import internally for `_module.args.pkgs`'s default; with our
    # `mkForce` override that default is never forced). Net cost: still one
    # nixpkgs import per eval.
    _module.args.pkgs = lib.mkForce (
      let
        hostPkgs = import nixpkgs {
          localSystem = {system = host;};
          inherit (config.nixpkgs) config overlays;
        };

        buildPlatformStdenvNoCC =
          hostPkgs.stdenvNoCC
          // {inherit (buildPkgs.stdenvNoCC) mkDerivation;};

        # A handful of packages are `callPackage`'d at hostPkgs construction
        # time and capture host-platform `replaceVarsWith`/`replaceVars`. They
        # set `allowSubstitutes = false` (forced inside `replaceVarsWith`),
        # which means cache substitution cannot recover their host-arch drvs
        # — they must be built on the build platform or fail. Output is a
        # text file with no architecture-specific runtime, so re-constructing
        # them via `.override` with build-platform helpers is safe.
        installerToolsShadow = let
          nixos-enter = hostPkgs.nixos-enter.override {
            replaceVarsWith = buildPkgs.replaceVarsWith;
          };
          nixos-install = hostPkgs.nixos-install.override {
            replaceVarsWith = buildPkgs.replaceVarsWith;
            inherit nixos-enter;
          };
          nixos-build-vms = hostPkgs.nixos-build-vms.override {
            replaceVarsWith = buildPkgs.replaceVarsWith;
          };
          # `buildEnv` builds via `stdenvNoCC.mkDerivation` and internally
          # uses `replaceVars ./builder.pl` plus `buildPackages.perl` to
          # assemble symlinks. All three need build-platform versions, or
          # the env-assembly drv is host-arch (aarch64) and its perl cannot
          # execute on the build host.
          #
          # `pkgs.buildEnv` uses `lib.makeOverridable` whose `.override` only
          # accepts the *inner* args (name, paths, ...), not callPackage's
          # outer args. Re-callPackage the buildenv source path to inject
          # the build-platform helpers we need.
          buildEnv = hostPkgs.callPackage (hostPkgs.path + "/pkgs/build-support/buildenv") {
            replaceVars = buildPkgs.replaceVars;
            stdenvNoCC = buildPlatformStdenvNoCC;
            buildPackages = buildPkgs.buildPackages;
          };

          # `nuke-references` provides the `nuke-refs` perl script via
          # `replaceVarsWith` (allowSubstitutes=false). It is consumed as a
          # `nativeBuildInputs` element by `makeModulesClosure` and other
          # helpers that strip Nix store references from artifacts. Output
          # is a perl text script — arch-portable.
          nuke-references =
            hostPkgs.callPackage
            (hostPkgs.path + "/pkgs/build-support/nuke-references") {
              replaceVarsWith = buildPkgs.replaceVarsWith;
              inherit (hostPkgs.darwin) signingUtils;
            };

          # `makeModulesClosure` shrinks a kernel modules tree using `kmod`
          # (modprobe) and `nuke-refs`. `kmod` reads ELF metadata
          # arch-independently, so build-platform `kmod` can introspect
          # aarch64 modules on x86_64 and produce the same modules.dep
          # text. Output is host-arch .ko files (substituted, not built)
          # plus arch-portable text (depmod info) — safe to flip the
          # assembly drv to system = build.
          makeModulesClosure = args:
            hostPkgs.callPackage
            (hostPkgs.path + "/pkgs/build-support/kernel/modules-closure.nix")
            (args
              // {
                stdenvNoCC = buildPlatformStdenvNoCC;
                kmod = buildPkgs.kmod;
                nukeReferences = nuke-references;
              });
        in {inherit nixos-enter nixos-install nixos-build-vms buildEnv nuke-references makeModulesClosure;};
      in
        hostPkgs
        // builtins.intersectAttrs buildAssemblyAttrNames buildPkgs
        // installerToolsShadow
        // {
          # `nixos/modules/system/activation/top-level.nix:58` builds
          # `system.build.toplevel` directly via `pkgs.stdenvNoCC.mkDerivation`,
          # bypassing the trivial-builder helpers. Shadow only the
          # `mkDerivation` entry point on `stdenvNoCC` so the toplevel and
          # any other direct-`mkDerivation` callsite emit `system = build`.
          # The rest of `stdenvNoCC` (hostPlatform, cc, helpers) stays bound
          # to the host pkgs — and because this shadow lives on
          # `_module.args.pkgs` (post-fixpoint), it does not cross-recurse
          # through nixpkgs internals like the overlay form did.
          stdenvNoCC = buildPlatformStdenvNoCC;
        }
    );
  };

  # Backwards-compatible no-op overlay alias. The fix-point overlay shape
  # cannot achieve the build/host split without breaking cache; callers that
  # need the seam should consume `mkBuildAssemblyPkgsModule` instead.
  mkBuildAssemblyOverlay = _: _final: _prev: {};
in {
  inherit
    buildAssemblyAttrNames
    mkBuildAssemblyPkgsModule
    mkBuildAssemblyOverlay
    ;
}
