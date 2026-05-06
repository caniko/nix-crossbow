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
    cache-shaped-with-cross-overrides = {
      name = "cache-shaped-with-cross-overrides";
      proves = "host-system closure keeps native cache shape while explicit packages may be cross-built";
      cacheExpectation = "high nixpkgs binary cache reuse; only opt-in package overrides use cross derivation paths";
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
    hardwareOptimization ? null,
    package,
    language ? "generic",
  }: let
    resolved = buildOptimizationProfileFor profile;
    hardwareProfile = hardwareProfileFor hardwareOptimization;
    hardwareFlags =
      if hardwareProfile == null
      then []
      else
        lib.optionals (hardwareProfile.platform.gcc ? arch) ["-march=${hardwareProfile.platform.gcc.arch}"]
        ++ lib.optionals (hardwareProfile.platform.gcc ? tune) ["-mtune=${hardwareProfile.platform.gcc.tune}"];
    metadata = {
      inherit language;
      profile = resolved.name or "custom";
      changesHashes = resolved.changesHashes or true;
    };
    hardwareMetadata =
      if hardwareProfile == null
      then null
      else hardwareProfile.name or "custom";
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
                hardwareOptimization = hardwareMetadata;
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
                  hardwareOptimization = hardwareMetadata;
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
            NIX_CFLAGS_COMPILE = appendString (old.NIX_CFLAGS_COMPILE or "") ((resolved.cFlags or []) ++ hardwareFlags);
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

  # Overlay that splits the closure along the stdenv/stdenvNoCC seam:
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
  # Caller passes an already-realised build-platform pkgs (e.g.
  # `nixpkgs.legacyPackages.${build}` from flake-parts' withSystem). We never
  # call `import nixpkgs { system = build; }` ourselves — that would double the
  # evaluation cost of every switch. Crossbow is a cheap optimisation path:
  # extra work versus a normal native NixOS eval is a handful of attribute
  # rebinds in trivial-builders.
  #
  # Set of trivial-builder attributes that NixOS modules use to assemble
  # text/symlink scaffolding (etc, units, activation scripts, registry JSON,
  # Home Manager wrappers, system.build.toplevel, …). Listed once and reused.
  buildAssemblyAttrNames = {
    runCommand = null;
    runCommandLocal = null;
    runCommandWith = null;
    writeText = null;
    writeTextFile = null;
    writeShellScript = null;
    writeShellScriptBin = null;
    writeShellApplication = null;
    symlinkJoin = null;
    linkFarm = null;
    linkFarmFromDrvs = null;
    concatTextFile = null;
    applyPatches = null;
    substituteAll = null;
    substitute = null;
    writeReferencesToFile = null;
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
      in
        hostPkgs
        // builtins.intersectAttrs buildAssemblyAttrNames buildPkgs
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
          stdenvNoCC =
            hostPkgs.stdenvNoCC
            // {inherit (buildPkgs.stdenvNoCC) mkDerivation;};
        }
    );
  };

  # Backwards-compatible no-op overlay alias. The fix-point overlay shape
  # cannot achieve the build/host split without breaking cache; callers that
  # need the seam should consume `mkBuildAssemblyPkgsModule` instead.
  mkBuildAssemblyOverlay = _: _final: _prev: {};

  # Mirror of nixos/modules/system/activation/top-level.nix's `baseSystem`
  # construction, but built via `buildPkgs.stdenvNoCC.mkDerivation` so the
  # resulting derivation has `system = build`. The script is intentionally
  # identical to nixpkgs upstream — it is shell that writes symlinks and
  # text files, identical content regardless of build platform — and the
  # references inside it (kernel, systemd, etc) remain native host paths
  # that substitute from cache.
  mkToplevelOverrideModule = {buildPkgs}: {
    config,
    lib,
    ...
  }: let
    inherit (lib) optionalString;
    systemBuilder = ''
      mkdir $out

      ${
        if config.boot.initrd.enable && config.boot.initrd.systemd.enable
        then ''
          cp "$systemd/lib/systemd/systemd" $out/init

          ${optionalString (!config.system.nixos-init.enable) ''
            cp ${config.system.build.bootStage2} $out/prepare-root
            substituteInPlace $out/prepare-root --subst-var-by systemConfig $out
          ''}
        ''
        else ''
          cp ${config.system.build.bootStage2} $out/init
          substituteInPlace $out/init --subst-var-by systemConfig $out
        ''
      }

      ln -s ${config.system.build.etc}/etc $out/etc

      ln -s ${config.system.path} $out/sw
      ln -s "$systemd" $out/systemd

      echo -n "systemd ${toString config.systemd.package.interfaceVersion}" > $out/init-interface-version
      echo -n "$nixosLabel" > $out/nixos-version
      echo -n "${config.boot.kernelPackages.stdenv.hostPlatform.system}" > $out/system

      ${config.system.systemBuilderCommands}

      cp "$extraDependenciesPath" "$out/extra-dependencies"

      ${optionalString (!config.boot.isContainer && config.boot.bootspec.enable) ''
        ${config.boot.bootspec.writer}
        ${optionalString config.boot.bootspec.enableValidation ''${config.boot.bootspec.validator} "$out/${config.boot.bootspec.filename}"''}
      ''}
    '';

    crossbowBaseSystem = buildPkgs.stdenvNoCC.mkDerivation (
      {
        name = "nixos-system-${config.system.name}-${config.system.nixos.label}";
        preferLocalBuild = true;
        allowSubstitutes = false;
        passAsFile = ["extraDependencies"];
        buildCommand = systemBuilder;

        systemd = config.systemd.package;

        nixosLabel = config.system.nixos.label;

        inherit (config.system) extraDependencies;
      }
      // config.system.systemBuilderArgs
    );
  in {
    system.build.toplevel = lib.mkForce (
      lib.asserts.checkAssertWarn config.assertions config.warnings crossbowBaseSystem
    );
  };

  mkNixosSwitchSystem = {
    nixpkgs ? inputs.nixpkgs,
    build,
    host,
    modules,
    specialArgs ? {},
    hardwareOptimization ? null,
    buildOptimization ? buildOptimizationProfiles.cache-first,
    # Pre-realised build-platform pkgs. Defaults to nixpkgs.legacyPackages, the
    # cached lazyAttrs flake-parts already builds — no second `import nixpkgs`.
    buildPkgs ? nixpkgs.legacyPackages.${build},
  }: let
    cacheMode = cacheModeFor "cache-shaped-with-cross-overrides";
    buildProfile = buildOptimizationProfileFor buildOptimization;
    profileName = hardwareOptimizationName hardwareOptimization;
    assemblyPkgsModule = mkBuildAssemblyPkgsModule {inherit nixpkgs host buildPkgs;};
  in
    nixpkgs.lib.nixosSystem {
      # Evaluate as a single-platform native host system: pkgs has cache-shape
      # names for every compiled package. The build/host split is achieved by
      # the stdenvNoCC overlay below, not by nixpkgs.buildPlatform.
      system = host;
      inherit specialArgs;
      modules =
        modules
        ++ [
          assemblyPkgsModule
          ({config, ...}: {
            nixpkgs.hostPlatform = lib.mkForce host;

            system.systemBuilderCommands = ''
              mkdir -p $out/nix-support
              cat > $out/nix-support/crossbow-mode <<'EOF'
              ${cacheMode.name}
              EOF
              cat > $out/nix-support/crossbow-cache-mode <<'EOF'
              ${cacheMode.name}
              EOF
              cat > $out/nix-support/crossbow-build-system <<'EOF'
              ${build}
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
                message = "crossbow NixOS switch must not rely on boot.binfmt.emulatedSystems for ${host}";
              }
            ];
          })
        ];
    };

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
            nixpkgs.hostPlatform = lib.mkForce host;

            system.systemBuilderCommands = ''
              mkdir -p $out/nix-support
              cat > $out/nix-support/crossbow-mode <<'EOF'
              ${cacheMode.name}
              EOF
              cat > $out/nix-support/crossbow-cache-mode <<'EOF'
              ${cacheMode.name}
              EOF
              cat > $out/nix-support/crossbow-build-system <<'EOF'
              ${build}
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

  mkCrossOverlay = {
    nixpkgs ? inputs.nixpkgs,
    build,
    host,
    overrides ? [],
    buildOptimization ? buildOptimizationProfiles.cache-first,
    hardwareOptimization ? null,
  }: let
    crossPkgs = import nixpkgs {
      system = build;
      crossSystem = host;
    };
    buildProfile = buildOptimizationProfileFor buildOptimization;
    hardwareProfile = hardwareProfileFor hardwareOptimization;
    mkOverrideModule = override:
      if lib.isFunction override
      then
        override {
          pkgs = crossPkgs;
          inherit build host;
          buildOptimization = buildProfile;
          hardwareOptimization = hardwareProfile;
          crossbow = self;
        }
      else override;
  in {
    imports = map mkOverrideModule overrides;
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
    profileName = hardwareOptimizationName hardwareOptimization;
  in
    nixpkgs.lib.nixosSystem {
      system = host;
      inherit specialArgs;
      modules =
        modules
        ++ [
          {
            nixpkgs.hostPlatform = lib.mkForce host;

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

  mkNixosCrossSystem = mkNixosSwitchSystem;

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
        mkBuildAssemblyOverlay
        mkBuildAssemblyPkgsModule
        mkToplevelOverrideModule
        mkCross
        mkCrossCheck
        mkNixosCrossSystem
        mkNixosSwitchSystem
        mkNixosStrictCrossSystem
        mkNixosNativeSubstitutedSystem
        mkCrossOverlay
        withCrossSupport
        ;

      executors = import ../executors {lib = self;};
      toolchains = import ../toolchains {lib = self;};
    };
in
  self
