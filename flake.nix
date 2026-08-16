{
  description = "QEMU-free cross-compilation helpers for Nix flakes";

  inputs = {
    rs-harbor.url = "git+https://codeberg.org/caniko/rs-harbor.git?ref=trunk&rev=c26b735eede8078f795651c4a9cbf0be8733b221";
    nixpkgs.follows = "rs-harbor/nixpkgs";
    rust-overlay.follows = "rs-harbor/rust-overlay";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      flake = {
        lib = import ./lib {inherit inputs;};
        nixosModules.cross-builder = import ./modules/cross-builder.nix;
      };

      perSystem = {
        pkgs,
        system,
        ...
      }: let
        formatter = pkgs.writeShellApplication {
          name = "crossbow-fmt";
          runtimeInputs = [pkgs.alejandra];
          text = ''
            if [ "$#" -eq 0 ]; then
              set -- .
            fi

            if [ "$#" -eq 1 ] && [ "$1" = "--check" ]; then
              set -- --check .
            fi

            exec alejandra "$@"
          '';
        };
        crossPackageProbeHost =
          if system == "x86_64-linux"
          then "aarch64-linux"
          else "x86_64-linux";
        crossPackageProbeOverrides = {
          hello =
            (import inputs.nixpkgs {
              localSystem = {inherit system;};
              crossSystem = {system = crossPackageProbeHost;};
            }).hello;
        };
        crossPackageProbe = inputs.self.lib.mkNixosSwitchSystem {
          build = system;
          host = crossPackageProbeHost;
          crossPackageAttrNames = ["hello"];
          crossPackageOverrides = crossPackageProbeOverrides;
          modules = [
            ({
              lib,
              pkgs,
              ...
            }: {
              options.crossbowProbe.package = lib.mkOption {
                type = lib.types.raw;
              };
              config = {
                system.stateVersion = "25.11";
                crossbowProbe.package = pkgs.hello;
              };
            })
          ];
        };
        crossPackageProbeRequirements = inputs.self.lib.mkNixosSwitchRequirements {
          buildPkgs = pkgs;
          host = crossPackageProbeHost;
          modules = [
            {
              boot.loader.grub.enable = false;
              fileSystems."/".device = "nodev";
              fileSystems."/".fsType = "tmpfs";
              system.stateVersion = "25.11";
            }
          ];
        };
        crossPackageProbeNative = inputs.self.lib.mkNixosNativeSubstitutedSystem {
          host = crossPackageProbeHost;
          modules = [
            {
              boot.loader.grub.enable = false;
              fileSystems."/".device = "nodev";
              fileSystems."/".fsType = "tmpfs";
              system.stateVersion = "25.11";
            }
          ];
        };
        crossRustProbeCrossPkgs = import inputs.nixpkgs {
          localSystem = {inherit system;};
          crossSystem = {system = "aarch64-linux";};
        };
        crossRustProbeCargoArtifacts = pkgs.stdenv.mkDerivation {
          pname = "cross-rust-probe-cargo-artifacts";
          version = "0";
          dontUnpack = true;
          installPhase = "mkdir -p $out";
          env.CARGO_ARTIFACTS_OLD_ENV = "preserved";
        };
        crossRustProbeBase = pkgs.stdenv.mkDerivation {
          pname = "cross-rust-probe";
          version = "0";
          dontUnpack = true;
          installPhase = "mkdir -p $out";
          nativeBuildInputs = [pkgs.hello];
          env.OLD_ENV = "preserved";
          cargoArtifacts = crossRustProbeCargoArtifacts;
        };
        crossRustProbePackage = inputs.self.lib.withCrossRust {
          crossbowCrossPkgs = crossRustProbeCrossPkgs;
          package = crossRustProbeBase;
          extraEnv.EXTRA_ENV = "merged";
          extraNativeBuildInputs = [pkgs.coreutils];
        };
        crossRustProbeEnv = (crossRustProbePackage.drvAttrs.env or {}) // crossRustProbePackage.drvAttrs;
        crossRustProbeNativeInputNames = map (input: input.pname or input.name or null) (crossRustProbePackage.drvAttrs.nativeBuildInputs or []);
        crossRustProbeCargoArtifactsEnv =
          if (crossRustProbePackage.drvAttrs ? cargoArtifacts) && (crossRustProbePackage.drvAttrs.cargoArtifacts ? drvAttrs)
          then (crossRustProbePackage.drvAttrs.cargoArtifacts.drvAttrs.env or {}) // crossRustProbePackage.drvAttrs.cargoArtifacts.drvAttrs
          else {};
        buildCache = inputs.rs-harbor.lib.mkBuildCachePolicy {
          inherit pkgs;
          buildPackageSet = pkgs.buildPackages;
          sccachePackage = pkgs.buildPackages.sccache;
          cacheRoot = null;
          namespaceScope = "canix-rust";
          namespaceGeneration = 5;
        };
        rustPkgs = import inputs.nixpkgs {
          localSystem = {inherit system;};
          overlays = [(import inputs.rust-overlay)];
        };
        toolchain = inputs.rs-harbor.lib.mkToolchain {
          pkgs = rustPkgs;
          toolchainProfile = "stable";
          cache.enable = false;
        };
      in {
        inherit formatter;

        packages = {
          crossbow-switch = buildCache.withRustCache {
            package =
              (rustPkgs.makeRustPlatform {
                rustc = toolchain.rustToolchain;
                cargo = toolchain.rustToolchain;
              }).buildRustPackage {
                pname = "crossbow-switch";
                version = "0.1.0";
                src = ./.;
                cargoLock.lockFile = ./Cargo.lock;
                cargoBuildFlags = ["-p" "crossbow-switch"];
                cargoTestFlags = ["-p" "crossbow-switch"];
              };
          };

          tiny-c-aarch64-linux = inputs.self.lib.mkCross {
            inherit pkgs;
            build = system;
            host = "aarch64-linux";
            package = import ./examples/tiny-c.nix;
            doCheck = false;
          };
        };

        apps.crossbow-switch = {
          type = "app";
          program = "${inputs.self.packages.${system}.crossbow-switch}/bin/crossbow-switch";
          meta.description = "Run cache-shaped Crossbow NixOS switch orchestration";
        };

        checks = {
          crossbow-switch = inputs.self.packages.${system}.crossbow-switch;

          pin-manifest = let
            manifest = inputs.self.lib.mkPinManifest {
              roots = [
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  arch = "aarch64-linux";
                  name = "direct";
                  policy = "must-substitute";
                  target = "target.direct";
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  hostSystem = "aarch64-linux";
                  host = "thething";
                  name = "assembled";
                  policy = "cross-build";
                  target = "target.assembled";
                  cachePackages = ["zeta"];
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  arch = "aarch64-linux";
                  name = "shared-one";
                  policy = "cross-build";
                  target = "target.shared-one";
                  cachePackages = ["shared"];
                  packageTargets.shared = "target.shared.one";
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  arch = "aarch64-linux";
                  name = "shared-two";
                  policy = "cross-build";
                  target = "target.shared-two";
                  cachePackages = ["shared"];
                  packageTargets.shared = "target.shared.two";
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  arch = "aarch64-linux";
                  name = "excluded";
                  policy = "build-platform";
                  target = "target.excluded";
                }
              ];
            };
            reordered = inputs.self.lib.mkPinManifest {
              roots = [
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  policy = "build-platform";
                  arch = "aarch64-linux";
                  name = "excluded";
                  target = "target.excluded";
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  policy = "cross-build";
                  arch = "aarch64-linux";
                  name = "shared-two";
                  target = "target.shared-two";
                  cachePackages = ["shared"];
                  packageTargets.shared = "target.shared.two";
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  policy = "must-substitute";
                  arch = "aarch64-linux";
                  name = "direct";
                  target = "target.direct";
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  policy = "cross-build";
                  hostSystem = "aarch64-linux";
                  host = "thething";
                  name = "assembled";
                  target = "target.assembled";
                  cachePackages = ["zeta"];
                }
                {
                  channel = "aarch64";
                  inputName = "nixpkgs";
                  policy = "cross-build";
                  arch = "aarch64-linux";
                  name = "shared-one";
                  target = "target.shared-one";
                  cachePackages = ["shared"];
                  packageTargets.shared = "target.shared.one";
                }
              ];
            };
          in pkgs.runCommand "crossbow-pin-manifest" {} ''
            test ${pkgs.lib.escapeShellArg (builtins.toJSON manifest)} = ${pkgs.lib.escapeShellArg (builtins.toJSON reordered)}
            test '${pkgs.lib.concatStringsSep " " manifest.groups.aarch64.packages}' = 'direct shared zeta'
            test '${manifest.groups.aarch64.consumerTargets.zeta}' = 'nixosConfigurations.thething-crossbow.pkgs.zeta'
            test '${manifest.groups.aarch64.requiredConsumerTargets."shared#1"}' = 'target.shared.two'
            test '${toString (builtins.length manifest.groups.aarch64.entries)}' = 4
            touch $out
          '';

          crossbow-metadata-pkl =
            pkgs.runCommand "crossbow-metadata-pkl" {
              buildInputs = [pkgs.diffutils pkgs.jq pkgs.pkl];
            } ''
              pkl_json="$(mktemp)"
              pkl eval -f json ${./data/CrossbowMetadata.pkl} | jq -S -c . > "$pkl_json"

              for sidecar in \
                ${./data/crossbow-metadata.json} \
                ${./crates/crossbow-switch/data/crossbow-metadata.json}
              do
                sidecar_json="$(mktemp)"
                jq -S -c . "$sidecar" > "$sidecar_json"
                if ! diff -u "$pkl_json" "$sidecar_json"; then
                  echo "ERROR: $sidecar is out of sync with data/CrossbowMetadata.pkl" >&2
                  echo "Regenerate with: pkl eval -f json data/CrossbowMetadata.pkl | jq . > data/crossbow-metadata.json" >&2
                  exit 1
                fi
              done

              touch "$out"
            '';

          platform-map = pkgs.runCommand "crossbow-platform-map" {} ''
            test "${inputs.self.lib.nixSystemToZigTarget "aarch64-linux"}" = "aarch64-linux-gnu"
            test "${inputs.self.lib.nixSystemToGnuConfig "aarch64-linux"}" = "aarch64-unknown-linux-gnu"
            test "${inputs.self.lib.targets.wasm32-wasi.config}" = "wasm32-wasi"
            test "${inputs.self.lib.cacheModes.strict-cross.name}" = "strict-cross"
            test "${inputs.self.lib.cacheModes.native-substituted.name}" = "native-substituted"
            test "${inputs.self.lib.cacheModes.cache-shaped-with-cross-overrides.name}" = "cache-shaped-with-cross-overrides"
            test "${
              if inputs.self.lib.buildOptimizationProfiles.cache-first.changesHashes
              then "1"
              else "0"
            }" = "0"
            test "${
              if inputs.self.lib.buildOptimizationProfiles.fast-local.changesHashes
              then "1"
              else "0"
            }" = "1"
            test "${
              if builtins.elem "-flto=thin" inputs.self.lib.buildOptimizationProfiles.fast-local.cFlags
              then "1"
              else "0"
            }" = "1"
            test "${
              if builtins.elem "-fuse-ld=mold" inputs.self.lib.buildOptimizationProfiles.fast-local.linkFlags
              then "1"
              else "0"
            }" = "1"
            test "${inputs.self.lib.hardwareProfiles.rockpro64.platform.gcc.tune}" = "cortex-a72.cortex-a53"
            test "${inputs.self.lib.hardwareOptimizationName "rockpro64"}" = "rockpro64"
            test "${(inputs.self.lib.mkOptimizedHostPlatform {
              host = "aarch64-linux";
              hardwareOptimization = "rockpro64";
            }).gcc.tune}" = "cortex-a72.cortex-a53"
            test "${
              (inputs.self.lib.selectOptimizedPkgs {
                enable = false;
                inherit pkgs;
              }).pkgs.stdenv.hostPlatform.system
            }" = "${pkgs.stdenv.hostPlatform.system}"
            test "${
              (inputs.self.lib.selectOptimizedPkgs {
                enable = true;
                inherit pkgs;
                crossbowCrossPkgs = import inputs.nixpkgs {
                  localSystem = {system = system;};
                  crossSystem = {system = "aarch64-linux";};
                };
              }).pkgs.stdenv.hostPlatform.system
            }" = "aarch64-linux"
            test "${
              if inputs.self.lib.mkNixosStrictCrossSystem == inputs.self.lib.mkNixosSwitchSystem
              then "1"
              else "0"
            }" = "0"
            test "${crossPackageProbe.config.crossbowProbe.package.stdenv.buildPlatform.system}" = "${system}"
            test "${crossPackageProbe.config.crossbowProbe.package.stdenv.hostPlatform.system}" = "${crossPackageProbeHost}"
            test -e ${crossPackageProbe.config.system.build.crossbowRequirements}/roots
            test -e ${crossPackageProbe.config.system.build.crossbowRequirements}/drvs
            test -e ${crossPackageProbeRequirements}/roots
            test -e ${crossPackageProbeRequirements}/drvs
            grep -Fx "${builtins.unsafeDiscardStringContext crossPackageProbeNative.config.system.build.toplevel}" ${crossPackageProbeRequirements}/roots >/dev/null
            grep -Fx "${builtins.unsafeDiscardStringContext crossPackageProbeNative.config.system.build.toplevel.drvPath}" ${crossPackageProbeRequirements}/drvs >/dev/null
            touch $out
          '';

          cross-rust-helper = pkgs.runCommand "cross-rust-helper" {} ''
            test "${crossRustProbeEnv.CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER or ""}" = "${crossRustProbeCrossPkgs.buildPackages.stdenv.cc}/bin/cc"
            test "${crossRustProbeEnv.HOST_CC or ""}" = "${crossRustProbeCrossPkgs.buildPackages.stdenv.cc}/bin/cc"
            test "${crossRustProbeEnv.CC_FOR_BUILD or ""}" = "${crossRustProbeCrossPkgs.buildPackages.stdenv.cc}/bin/cc"
            test "${crossRustProbeEnv.OLD_ENV or ""}" = "preserved"
            test "${crossRustProbeEnv.EXTRA_ENV or ""}" = "merged"
            test "${crossRustProbeCargoArtifactsEnv.CARGO_ARTIFACTS_OLD_ENV or ""}" = "preserved"
            test "${crossRustProbeCargoArtifactsEnv.CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER or ""}" = "${crossRustProbeCrossPkgs.buildPackages.stdenv.cc}/bin/cc"
            ${
              if builtins.elem "hello" crossRustProbeNativeInputNames && builtins.elem "coreutils" crossRustProbeNativeInputNames
              then "true"
              else "echo 'cross-rust-helper nativeBuildInputs were not preserved and extended'; exit 1"
            }
            touch $out
          '';

          tiny-c-aarch64-linux = inputs.self.packages.${system}.tiny-c-aarch64-linux;

          unsupported-linux-darwin-message = pkgs.runCommand "crossbow-unsupported-linux-darwin-message" {} ''
            message='${builtins.replaceStrings ["'"] ["'\\''"] (inputs.self.lib.unsupportedToolchainMessage {
              build = "x86_64-linux";
              host = "aarch64-darwin";
            })}'
            printf '%s' "$message" | grep -F "darwinSdk" >/dev/null
            touch $out
          '';
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.alejandra
            pkgs.cargo
            pkgs.clippy
            pkgs.nix
            pkgs.rustc
            pkgs.rustfmt
            pkgs.zig
          ];
        };
      };
    };
}
