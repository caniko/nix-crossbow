{
  lib,
  self,
  inputs,
  cacheModeFor,
  buildOptimizationProfiles,
  buildOptimizationProfileFor,
  hardwareProfileFor,
  hardwareOptimizationName,
  mkBuildAssemblyPkgsModule,
}: let
  # Emit `$out/nix-support/crossbow-*` metadata files inside any
  # `system.systemBuilderCommands` block. Centralised so all three
  # constructors agree on the file format.
  #
  # `crossbow-mode` and `crossbow-build-system` are skipped when their
  # value is null (used by `mkNixosNativeSubstitutedSystem` to preserve
  # its prior, narrower metadata surface).
  mkCrossbowMetadataCommands = {
    cacheMode,
    build ? null,
    buildProfile,
    profileName,
    emitModeFile ? true,
  }: ''
    mkdir -p $out/nix-support
    ${lib.optionalString emitModeFile ''
      cat > $out/nix-support/crossbow-mode <<'EOF'
      ${cacheMode.name}
      EOF
    ''}
    cat > $out/nix-support/crossbow-cache-mode <<'EOF'
    ${cacheMode.name}
    EOF
    ${lib.optionalString (build != null) ''
      cat > $out/nix-support/crossbow-build-system <<'EOF'
      ${build}
      EOF
    ''}
    cat > $out/nix-support/crossbow-build-optimization <<'EOF'
    ${buildProfile.name}
    EOF
    ${lib.optionalString (profileName != null) ''
      cat > $out/nix-support/crossbow-hardware-optimization <<'EOF'
      ${profileName}
      EOF
    ''}
  '';

  binfmtAssertion = host: kind: {config, ...}: {
    assertions = [
      {
        assertion = !lib.elem host (config.boot.binfmt.emulatedSystems or []);
        message = "crossbow ${kind} must not rely on boot.binfmt.emulatedSystems for ${host}";
      }
    ];
  };

  mkRequirementsArtifact = {
    buildPkgs,
    roots ? [],
    rootLabels ? [],
  }: let
    fingerprint = builtins.hashString "sha256"
      (lib.concatStringsSep "\n" (map builtins.unsafeDiscardStringContext roots));
    rootFingerprints =
      if roots == [] || rootLabels == []
      then []
      else map (root: builtins.hashString "sha256" (builtins.unsafeDiscardStringContext root)) roots;
  in
    buildPkgs.runCommand "crossbow-switch-requirements" {} ''
      mkdir -p "$out/nix-support"
      cat > "$out/roots" <<'EOF'
      ${lib.concatStringsSep "\n" (map builtins.unsafeDiscardStringContext roots)}
      EOF
      cat > "$out/drvs" <<'EOF'
      ${lib.concatStringsSep "\n" (map (root: builtins.unsafeDiscardStringContext root.drvPath) roots)}
      EOF
      cat > "$out/fingerprint" <<'EOF'
      ${fingerprint}
      EOF
      ${lib.optionalString (rootLabels != []) ''
        cat > "$out/root-labels" <<'EOF'
        ${lib.concatStringsSep "\n" rootLabels}
        EOF
      ''}
      ${lib.optionalString (rootFingerprints != []) ''
        cat > "$out/root-fingerprints" <<'EOF'
        ${lib.concatStringsSep "\n" rootFingerprints}
        EOF
      ''}
      cp "$out/roots" "$out/nix-support/crossbow-requirements"
      cp "$out/drvs" "$out/nix-support/crossbow-requirement-drvs"
      cp "$out/fingerprint" "$out/nix-support/crossbow-requirements-fingerprint"
    '';

  mkEmptyNixosSwitchRequirements = {buildPkgs}: mkRequirementsArtifact {inherit buildPkgs;};

  mkNixosSwitchRequirements = {
    nixpkgs ? inputs.nixpkgs,
    host,
    modules,
    specialArgs ? {},
    hardwareOptimization ? null,
    buildOptimization ? buildOptimizationProfiles.cache-first,
    buildPkgs ? null,
    crossPackageAttrNames ? [],
  }: let
    nativeSystem = mkNixosNativeSubstitutedSystem {
      inherit
        nixpkgs
        host
        modules
        specialArgs
        hardwareOptimization
        buildOptimization
        ;
    };
    artifactPkgs =
      if buildPkgs != null
      then buildPkgs
      else nativeSystem.pkgs;
    # Cross-compiled packages as additional roots. Only computed when
    # buildPkgs is present (cross-compilation needs a build platform).
    hasCrossPackages = buildPkgs != null && crossPackageAttrNames != [];
    crossPkgs =
      if hasCrossPackages
      then import nixpkgs {
        localSystem = {system = artifactPkgs.stdenv.buildPlatform.system;};
        crossSystem = {system = host;};
        inherit (nativeSystem.config.nixpkgs) config overlays;
      }
      else {};
    crossPackageLabels = lib.filter (name: crossPkgs ? ${name}) crossPackageAttrNames;
    crossPackageRoots = map (name: crossPkgs.${name}) crossPackageLabels;
    allRoots = [nativeSystem.config.system.build.toplevel] ++ crossPackageRoots;
    allLabels = ["system-toplevel"] ++ crossPackageLabels;
  in
    mkRequirementsArtifact {
      buildPkgs = artifactPkgs;
      roots = allRoots;
      rootLabels = allLabels;
    };

  mkNixosSwitchSystem = {
    nixpkgs ? inputs.nixpkgs,
    build,
    host,
    modules,
    specialArgs ? {},
    hardwareOptimization ? null,
    buildOptimization ? buildOptimizationProfiles.cache-first,
    crossPackageAttrNames ? [],
    # Pre-realised build-platform pkgs. Defaults to nixpkgs.legacyPackages, the
    # cached lazyAttrs flake-parts already builds — no second `import nixpkgs`.
    buildPkgs ? nixpkgs.legacyPackages.${build},
  }: let
    cacheMode = cacheModeFor "cache-shaped-with-cross-overrides";
    buildProfile = buildOptimizationProfileFor buildOptimization;
    profileName = hardwareOptimizationName hardwareOptimization;
    assemblyPkgsModule = mkBuildAssemblyPkgsModule {inherit nixpkgs build host buildPkgs crossPackageAttrNames;};
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
          ({
            config,
            lib,
            ...
          }: {
            # Provide a `crossbowCrossPkgs` module arg for callers that want to
            # build specific packages (Go, Rust, …) cross-compiled on the build
            # platform instead of natively on host. Caches differ from
            # cache.nixos.org for cross builds, but that's already accepted by
            # callers who opt in (e.g. via canix `applyBuildOptimization` paths
            # that inspect `crossbowCrossPkgs != null`).
            _module.args.crossbowCrossPkgs = lib.mkOverride 999 (import nixpkgs {
              localSystem = {system = build;};
              crossSystem = {system = host;};
              inherit (config.nixpkgs) config overlays;
            });
            _module.args.crossbowBuildPkgs = lib.mkOverride 999 buildPkgs;

            nixpkgs.hostPlatform = lib.mkForce host;

            system.build.crossbowRequirements = lib.mkDefault (
              mkEmptyNixosSwitchRequirements {inherit buildPkgs;}
            );

            system.systemBuilderCommands = mkCrossbowMetadataCommands {
              inherit cacheMode build buildProfile profileName;
            };
          })
          (binfmtAssertion host "NixOS switch")
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
          {
            nixpkgs.buildPlatform = lib.mkForce build;
            nixpkgs.hostPlatform = lib.mkForce host;

            system.systemBuilderCommands = mkCrossbowMetadataCommands {
              inherit cacheMode build buildProfile profileName;
            };
          }
          (binfmtAssertion host "strict NixOS proof")
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

            system.systemBuilderCommands = mkCrossbowMetadataCommands {
              inherit cacheMode buildProfile profileName;
              # `crossbow-mode` is omitted to match the prior behaviour
              # of this constructor (which only emitted `crossbow-cache-mode`).
              emitModeFile = false;
            };
          }
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

  mkNixosCrossSystem = mkNixosSwitchSystem;
in {
  inherit
    mkNixosSwitchSystem
    mkNixosSwitchRequirements
    mkNixosStrictCrossSystem
    mkNixosNativeSubstitutedSystem
    mkNixosCrossSystem
    mkCrossOverlay
    ;
}
