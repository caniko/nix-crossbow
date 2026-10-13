# crossbow lib: orchestration only.
#
# Each submodule is self-contained and takes its dependencies (lib, self,
# inputs, peer helpers) as explicit args. `self` is the public crossbow API
# attrset, threaded via fixed-point so submodules can reference one another
# (e.g. cross-package needs selectToolchain from toolchain).
{inputs}: let
  nixpkgs = inputs.nixpkgs;
  lib = nixpkgs.lib;

  platformMap = import ./platform-map.nix {inherit lib;};
  targets = import ./targets.nix;

  platforms = import ./platforms.nix {inherit lib targets;};
  cacheModes = import ./cache-modes.nix {inherit lib;};
  hardwareProfiles = import ./hardware-profiles.nix {
    inherit lib;
    inherit (platforms) targetFor;
  };
  buildOptimization = import ./build-optimization.nix {
    inherit lib;
    inherit (hardwareProfiles) hardwareProfileFor;
  };
  buildAssemblyPkgs = import ./build-assembly-pkgs.nix {inherit lib;};
  pinManifest = import ./pin-manifest.nix {inherit lib;};
  crossRust = import ./cross-rust.nix {inherit lib self;};
  toplevelOverride = import ./toplevel-override.nix {};

  self =
    platformMap
    // {
      inherit targets;
      inherit (cacheModes) cacheModes cacheModeFor;
      inherit
        (buildOptimization)
        buildOptimizationProfiles
        buildOptimizationProfileFor
        selectOptimizedPkgs
        applyBuildOptimization
        ;
      inherit
        (hardwareProfiles)
        hardwareProfiles
        hardwareProfileFor
        mkOptimizedHostPlatform
        hardwareOptimizationName
        ;
      inherit (toolchain) unsupportedToolchainMessage selectToolchain;
      inherit (crossPackage) mkCross mkCrossCheck;
      inherit (crossRust) withCrossRust;
      inherit
        (buildAssemblyPkgs)
        mkBuildAssemblyOverlay
        mkBuildAssemblyPkgsModule
        ;
      inherit (toplevelOverride) mkToplevelOverrideModule;
      inherit
        (nixosSystems)
        mkNixosSwitchSystem
        mkNixosSwitchRequirements
        mkNixosStrictCrossSystem
        mkNixosNativeSubstitutedSystem
        mkNixosCrossSystem
        mkCrossOverlay
        ;
      inherit (withCrossSupport) withCrossSupport;
      inherit (pinManifest) mkPinManifest;

      executors = import ../executors {lib = self;};
      toolchains = import ../toolchains {lib = self;};
    };

  # Submodules with `self` dependencies — declared after `self` so the
  # fixed-point can resolve. Nix lazy bindings make the apparent cycle
  # safe: each submodule only reads the parts of `self` it needs.
  toolchain = import ./toolchain.nix {
    inherit self;
    inherit (platforms) isLinux isDarwin isWindows isWasm;
  };
  crossPackage = import ./cross-package.nix {
    inherit lib self;
    inherit (platforms) targetFor;
    inherit (toolchain) selectToolchain;
  };
  nixosSystems = import ./nixos-systems.nix {
    inherit lib self inputs;
    inherit (cacheModes) cacheModeFor;
    inherit (buildOptimization) buildOptimizationProfiles buildOptimizationProfileFor;
    inherit (hardwareProfiles) hardwareProfileFor hardwareOptimizationName;
    inherit (buildAssemblyPkgs) mkBuildAssemblyPkgsModule;
  };
  withCrossSupport = import ./with-cross-support.nix {
    inherit lib;
    inherit (crossPackage) mkCross;
  };
in
  self
