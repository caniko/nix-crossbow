{inputs}: let
  nixpkgs = inputs.nixpkgs;
  lib = nixpkgs.lib;

  platformMap = import ./platform-map.nix {inherit lib;};
  targets = import ./targets.nix;

  targetFor = system:
    targets.${system}
    or (throw "crossbow: unsupported host platform `${system}`; add it to lib/targets.nix and lib/platform-map.nix first");

  osOf = system: let
    parts = lib.splitString "-" system;
  in
    lib.last parts;

  isLinux = system: osOf system == "linux";
  isDarwin = system: osOf system == "darwin";
  isWindows = system: osOf system == "windows";
  isWasm = system: lib.hasPrefix "wasm" system;

  cacheModes = {
    strict-cross = {
      name = "strict-cross";
      proves = "build machine compiles host artifacts without QEMU";
      cacheExpectation = "low nixpkgs binary cache reuse because cross derivation paths differ from native paths";
    };
    native-substituted = {
      name = "native-substituted";
      proves = "build machine can orchestrate and substitute host-system artifacts without QEMU";
      cacheExpectation = "high nixpkgs binary cache reuse when host-system paths are available from substituters";
    };
    remote-native = {
      name = "remote-native";
      proves = "build machine can route missing host-system builds to native hardware without QEMU";
      cacheExpectation = "uses substitutes first, then a native remote builder for missing host-system paths";
    };
  };

  cacheModeFor = mode:
    cacheModes.${mode}
    or (throw "crossbow: unsupported cache mode `${mode}`; expected one of ${lib.concatStringsSep ", " (builtins.attrNames cacheModes)}");

  buildOptimizationProfiles = {
    cache-first = {
      name = "cache-first";
      changesHashes = false;
      cFlags = [];
      linkFlags = [];
      rustFlags = [];
      goFlags = [];
      nativeBuildInputs = pkgs: [];
      description = "Preserve nixpkgs derivation hashes and maximize binary cache reuse.";
    };
    fast-local = {
      name = "fast-local";
      changesHashes = true;
      cFlags = ["-flto=thin"];
      linkFlags = ["-fuse-ld=mold"];
      rustFlags = [
        "-C"
        "lto=thin"
        "-C"
        "link-arg=-fuse-ld=mold"
      ];
      goFlags = ["-trimpath"];
      nativeBuildInputs = pkgs: [
        pkgs.mold
      ];
      description = "Use local build accelerators for derivations that are not expected to substitute.";
    };
  };

  buildOptimizationProfileFor = profile:
    if profile == null
    then buildOptimizationProfiles.cache-first
    else if lib.isString profile
    then buildOptimizationProfiles.${profile}
      or (throw "crossbow: unsupported build optimization profile `${profile}`; expected one of ${lib.concatStringsSep ", " (builtins.attrNames buildOptimizationProfiles)}")
    else profile;

  appendString = oldValue: values: let
    newValue = lib.concatStringsSep " " values;
  in
    lib.concatStringsSep " " (lib.filter (value: value != "") [oldValue newValue]);

  appendList = oldValue: values: oldValue ++ values;

  applyBuildOptimization = {
    pkgs,
    profile ? buildOptimizationProfiles.cache-first,
    package,
    language ? "generic",
  }: let
    resolved = buildOptimizationProfileFor profile;
    metadata = {
      inherit language;
      profile = resolved.name or "custom";
      changesHashes = resolved.changesHashes or true;
    };
  in
    if !(resolved.changesHashes or true)
    then
      package
      // {
        passthru =
          (package.passthru or {})
          // {
            crossbow =
              (package.passthru.crossbow or {})
              // {
                buildOptimization = metadata;
              };
          };
      }
    else
      package.overrideAttrs (old: let
        nativeInputs = resolved.nativeBuildInputs or (_: []);
        optimizedNativeBuildInputs =
          if language == "go"
          then []
          else nativeInputs pkgs;
        commonAttrs = {
          nativeBuildInputs = appendList (old.nativeBuildInputs or []) optimizedNativeBuildInputs;
          passthru =
            (old.passthru or {})
            // {
              crossbow =
                ((old.passthru or {}).crossbow or {})
                // {
                  buildOptimization = metadata;
                };
            };
        };
      in
        commonAttrs
        // (
          if language == "go"
          then {
            enableParallelBuilding = old.enableParallelBuilding or true;
          }
          else if language == "rust"
          then {
            RUSTFLAGS = appendString (old.RUSTFLAGS or "") (resolved.rustFlags or []);
            CARGO_PROFILE_RELEASE_LTO = old.CARGO_PROFILE_RELEASE_LTO or "thin";
            CARGO_PROFILE_RELEASE_CODEGEN_UNITS = old.CARGO_PROFILE_RELEASE_CODEGEN_UNITS or "1";
          }
          else {
            NIX_CFLAGS_COMPILE = appendString (old.NIX_CFLAGS_COMPILE or "") (resolved.cFlags or []);
            NIX_LDFLAGS = appendString (old.NIX_LDFLAGS or "") (resolved.linkFlags or []);
          }
        ));

  hardwareProfiles = {
    rockpro64 = {
      name = "rockpro64";
      system = "aarch64-linux";
      vendor = "pine64";
      soc = "rk3399";
      description = "Pine64 RockPro64 / RK3399: Cortex-A72 + Cortex-A53 big.LITTLE ARMv8-A";
      platform = {
        gcc = {
          arch = "armv8-a";
          tune = "cortex-a72.cortex-a53";
        };
      };
    };
  };

  hardwareProfileFor = hardwareOptimization:
    if hardwareOptimization == null
    then null
    else if lib.isString hardwareOptimization
    then hardwareProfiles.${hardwareOptimization}
      or (throw "crossbow: unsupported hardware optimization profile `${hardwareOptimization}`; expected one of ${lib.concatStringsSep ", " (builtins.attrNames hardwareProfiles)}")
    else hardwareOptimization;

  mkOptimizedHostPlatform = {
    host,
    hardwareOptimization ? null,
  }: let
    hostTarget = targetFor host;
    profile = hardwareProfileFor hardwareOptimization;
  in
    if profile == null
    then host
    else if (profile.system or host) != host
    then throw "crossbow: hardware optimization profile `${profile.name or "<unnamed>"}` is for `${profile.system}` but host is `${host}`"
    else
      hostTarget
      // (profile.platform or {})
      // {
        system = host;
        config = hostTarget.config;
      };

  hardwareOptimizationName = hardwareOptimization: let
    profile = hardwareProfileFor hardwareOptimization;
  in
    if profile == null
    then null
    else profile.name or "custom";

  unsupportedToolchainMessage = {
    build,
    host,
    darwinSdk ? null,
  }:
    if isWasm host
    then "crossbow: wasm toolchain is declared but not implemented in phase 1"
    else if isWindows host
    then "crossbow: windows toolchain is declared but not implemented in phase 1"
    else if isLinux build && isDarwin host && darwinSdk == null
    then ''
      crossbow: linux-to-darwin cross-compilation requires darwinSdk.
      Set darwinSdk = /path/to/MacOSX.sdk and configure osxcross.
    ''
    else if isDarwin host
    then "crossbow: darwin toolchain is declared but not implemented in phase 1"
    else "crossbow: unsupported toolchain pair `${build}` -> `${host}`";

  selectToolchain = {
    build,
    host,
    pkgs,
    darwinSdk ? null,
  }:
    if isLinux build && isLinux host
    then
      import ../toolchains/linux-linux.nix {
        inherit pkgs;
        crossbow = self;
        buildSystem = build;
        hostSystem = host;
      }
    else if isWasm host
    then throw (unsupportedToolchainMessage {inherit build host darwinSdk;})
    else if isWindows host
    then throw (unsupportedToolchainMessage {inherit build host darwinSdk;})
    else if isLinux build && isDarwin host && darwinSdk == null
    then throw (unsupportedToolchainMessage {inherit build host darwinSdk;})
    else if isDarwin host
    then throw (unsupportedToolchainMessage {inherit build host darwinSdk;})
    else throw (unsupportedToolchainMessage {inherit build host darwinSdk;});

  mkCross = {
    pkgs,
    host,
    package,
    build ? pkgs.stdenv.buildPlatform.system,
    target ? null,
    toolchain ?
      selectToolchain {
        inherit build host pkgs;
      },
    doCheck ? true,
    checkRunner ? null,
    darwinSdk ? null,
  }: let
    hostTarget = targetFor host;
    out =
      if lib.isFunction package
      then
        package {
          inherit pkgs toolchain;
          buildSystem = build;
          hostSystem = host;
          targetSystem = target;
          stdenv = pkgs.stdenv;
        }
      else
        import package {
          inherit pkgs toolchain;
          buildSystem = build;
          hostSystem = host;
          targetSystem = target;
          stdenv = pkgs.stdenv;
        };
    check =
      if doCheck
      then
        mkCrossCheck {
          inherit pkgs out;
          host = hostTarget;
          executor =
            if checkRunner != null
            then checkRunner
            else self.executors.native-builder {};
        }
      else null;
  in
    out
    // {
      passthru =
        (out.passthru or {})
        // {
          crossbow = {
            inherit build host target toolchain check;
          };
        };
    };

  mkCrossCheck = {
    pkgs,
    out,
    host,
    executor ? self.executors.native-builder {},
  }: let
    checkName = out.name or out.pname or "crossbow";
  in
    if executor.kind == "skip"
    then
      pkgs.runCommand "${checkName}-check-skipped" {
        passthru.crossbow.skipReason = executor.reason;
      } ''
        printf '%s\n' '${executor.reason}' > $out
      ''
    else if executor.kind == "native-builder"
    then
      pkgs.runCommand "${checkName}-native-builder-check" {
        passthru.crossbow.requiredSystem = host.system;
      } ''
        test -n '${host.system}'
        touch $out
      ''
    else throw "crossbow: executor `${executor.kind}` is not implemented in phase 1";

  mkNixosStrictCrossSystem = {
    nixpkgs ? inputs.nixpkgs,
    build,
    host,
    modules,
    specialArgs ? {},
    hardwareOptimization ? null,
    buildOptimization ? buildOptimizationProfiles.cache-first,
  }: let
    cacheMode = cacheModeFor "strict-cross";
    buildProfile = buildOptimizationProfileFor buildOptimization;
    hostPlatform = mkOptimizedHostPlatform {
      inherit host hardwareOptimization;
    };
    profileName = hardwareOptimizationName hardwareOptimization;
  in
    nixpkgs.lib.nixosSystem {
      system = build;
      inherit specialArgs;
      modules =
        modules
        ++ [
          ({config, ...}: {
            nixpkgs.buildPlatform = lib.mkForce build;
            nixpkgs.hostPlatform = lib.mkForce hostPlatform;

            system.systemBuilderCommands = ''
              mkdir -p $out/nix-support
              cat > $out/nix-support/crossbow-cache-mode <<'EOF'
              ${cacheMode.name}
              EOF
              cat > $out/nix-support/crossbow-build-optimization <<'EOF'
              ${buildProfile.name}
              EOF
              ${lib.optionalString (profileName != null) ''
                cat > $out/nix-support/crossbow-hardware-optimization <<'EOF'
                ${profileName}
                EOF
              ''}
            '';

            assertions = [
              {
                assertion = !lib.elem host (config.boot.binfmt.emulatedSystems or []);
                message = "crossbow strict NixOS proof must not rely on boot.binfmt.emulatedSystems for ${host}";
              }
            ];
          })
        ];
    };

  mkNixosNativeSubstitutedSystem = {
    nixpkgs ? inputs.nixpkgs,
    host,
    modules,
    specialArgs ? {},
    hardwareOptimization ? null,
    buildOptimization ? buildOptimizationProfiles.cache-first,
  }: let
    cacheMode = cacheModeFor "native-substituted";
    buildProfile = buildOptimizationProfileFor buildOptimization;
    hostPlatform = mkOptimizedHostPlatform {
      inherit host hardwareOptimization;
    };
    profileName = hardwareOptimizationName hardwareOptimization;
  in
    nixpkgs.lib.nixosSystem {
      system = host;
      inherit specialArgs;
      modules =
        modules
        ++ [
          {
            nixpkgs.hostPlatform = lib.mkForce hostPlatform;

            system.systemBuilderCommands = ''
              mkdir -p $out/nix-support
              cat > $out/nix-support/crossbow-cache-mode <<'EOF'
              ${cacheMode.name}
              EOF
              cat > $out/nix-support/crossbow-build-optimization <<'EOF'
              ${buildProfile.name}
              EOF
              ${lib.optionalString (profileName != null) ''
                cat > $out/nix-support/crossbow-hardware-optimization <<'EOF'
                ${profileName}
                EOF
              ''}
            '';
          }
        ];
    };

  mkNixosCrossSystem = mkNixosStrictCrossSystem;

  withCrossSupport = {
    inputs,
    buildSystems,
    crossTargets,
    packages,
    darwinSdk ? null,
  }: let
    forBuildSystem = build: let
      pkgs = inputs.nixpkgs.legacyPackages.${build};
      native = packages pkgs;
      crossForPackage = name: package:
        lib.listToAttrs (map (target: {
            name = "${name}-${target.system}";
            value = mkCross {
              inherit pkgs build package darwinSdk;
              host = target.system;
              doCheck = false;
            };
          })
          crossTargets);
    in
      native // lib.concatMapAttrs crossForPackage native;
  in {
    packages = lib.genAttrs buildSystems forBuildSystem;
  };

  self =
    platformMap
    // {
      inherit
        targets
        cacheModes
        cacheModeFor
        buildOptimizationProfiles
        buildOptimizationProfileFor
        applyBuildOptimization
        hardwareProfiles
        hardwareProfileFor
        mkOptimizedHostPlatform
        hardwareOptimizationName
        unsupportedToolchainMessage
        selectToolchain
        mkCross
        mkCrossCheck
        mkNixosCrossSystem
        mkNixosStrictCrossSystem
        mkNixosNativeSubstitutedSystem
        withCrossSupport
        ;

      executors = import ../executors {lib = self;};
      toolchains = import ../toolchains {lib = self;};
    };
in
  self
