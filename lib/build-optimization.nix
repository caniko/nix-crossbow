{
  lib,
  hardwareProfileFor,
}: let
  buildOptimizationProfiles = {
    cache-first = {
      name = "cache-first";
      changesHashes = false;
      cFlags = [];
      linkFlags = [];
      rustFlags = [];
      goFlags = [];
      goLdflags = [];
      goEnv = {};
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
      # Go's compiler does its own linking and ignores `-fuse-ld=mold`;
      # `-trimpath` shaves stat/path-canonicalisation work off every package
      # while keeping output identical modulo embedded paths.
      goFlags = ["-trimpath"];
      goLdflags = ["-s" "-w"];
      # `CGO_ENABLED=0` skips C toolchain setup and the cgo link step. For
      # pure-Go packages (the common case — caddy, most plugins) this is a
      # straight 10–30% wallclock win and produces a static binary, so
      # `nuke-references` has less to do downstream too. CGo-using packages
      # opt out by setting `goEnv = {}` on a custom profile.
      goEnv = {CGO_ENABLED = "0";};
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

  # Whitespace-tokenise an existing flag string and skip values already
  # present, so re-applying optimization on a package that already sets the
  # same flag (e.g. nixpkgs caddy already uses `-trimpath`) doesn't produce
  # duplicates like "-trimpath -trimpath".
  appendString = oldValue: values: let
    existing = lib.filter (s: s != "") (lib.splitString " " oldValue);
    novel = lib.filter (v: !(lib.elem v existing)) values;
  in
    lib.concatStringsSep " " (existing ++ novel);

  appendList = oldValue: values: oldValue ++ (lib.filter (v: !(lib.elem v oldValue)) values);

  selectOptimizedPkgs = {
    enable,
    pkgs,
    crossbowCrossPkgs ? null,
    consumerName ? "crossbow optimized package selection",
  }: let
    useCrossPkgs = enable && crossbowCrossPkgs != null;
    selectedPkgs =
      if useCrossPkgs
      then crossbowCrossPkgs
      else pkgs;
  in {
    inherit selectedPkgs useCrossPkgs;
    pkgs = selectedPkgs;
    assertions = lib.optional useCrossPkgs {
      assertion =
        crossbowCrossPkgs.stdenv.buildPlatform.system
        != crossbowCrossPkgs.stdenv.hostPlatform.system;
      message = ''
        ${consumerName}: crossbowCrossPkgs is configured natively
        (buildPlatform == hostPlatform == ${crossbowCrossPkgs.stdenv.hostPlatform.system}).
        Packages selected through crossbowCrossPkgs must be cross-compiled,
        not emulated. Either disable the optimized package selection for this
        consumer, or pass a crossbowCrossPkgs whose buildPlatform differs from
        hostPlatform.
      '';
    };
  };

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
        # Go has its own toolchain linker — mold is irrelevant for pure-Go
        # builds. CGo can benefit from mold, but plumbing it via
        # `nativeBuildInputs` here would change derivation hashes for every
        # Go package without measurable gain on the targets we care about.
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
          then let
            goEnv = resolved.goEnv or {};
            # Merge `GOFLAGS` into whichever form the underlying derivation
            # already uses. Pure-`env` packages (structured-attrs) keep
            # `env.GOFLAGS`; legacy mkDerivation packages (caddy as of writing)
            # keep top-level `GOFLAGS`. Setting both raises a build-time
            # "overlapping attributes" error.
            usesEnv = (old ? env) && (old.env ? GOFLAGS);
            mergedGoflags = appendString (
              if usesEnv
              then old.env.GOFLAGS
              else old.GOFLAGS or ""
            ) (resolved.goFlags or []);
            # `goEnv` entries (e.g. CGO_ENABLED=0) override any existing
            # value on the package — the optimization profile is the
            # authoritative source for these knobs. Land them in `env` if
            # the package uses structured-attrs, otherwise as top-level
            # drv attrs.
          in
            {
              enableParallelBuilding = old.enableParallelBuilding or true;
              # `ldflags` is appended to the final link step (e.g. `-s -w` to
              # strip symbol/debug info).
              ldflags = appendList (old.ldflags or []) (resolved.goLdflags or []);
            }
            // (
              if usesEnv
              then {env = old.env // {GOFLAGS = mergedGoflags;} // goEnv;}
              else {GOFLAGS = mergedGoflags;} // goEnv
            )
          else if language == "rust"
          then let
            # Rust packages may set `RUSTFLAGS`, `CARGO_PROFILE_*` and
            # `NIX_CFLAGS_COMPILE` in `env` (structured-attrs, e.g.
            # nixpkgs vector) or as top-level mkDerivation args
            # (legacy). Setting both raises a build-time "overlapping
            # attributes" error, so detect and merge into the form the
            # package already uses.
            usesEnv = (old ? env) && lib.any (k: old.env ? ${k}) ["RUSTFLAGS" "CARGO_PROFILE_RELEASE_LTO" "CARGO_PROFILE_RELEASE_CODEGEN_UNITS" "NIX_CFLAGS_COMPILE"];
            getOld = k:
              if usesEnv && old.env ? ${k}
              then old.env.${k}
              else old.${k} or "";
            mergedRustflags = appendString (getOld "RUSTFLAGS") (resolved.rustFlags or []);
            mergedCflags = appendString (getOld "NIX_CFLAGS_COMPILE") hardwareFlags;
            cargoLto = getOld "CARGO_PROFILE_RELEASE_LTO";
            cargoCgUnits = getOld "CARGO_PROFILE_RELEASE_CODEGEN_UNITS";
            rustEnv = {
              RUSTFLAGS = mergedRustflags;
              CARGO_PROFILE_RELEASE_LTO =
                if cargoLto != ""
                then cargoLto
                else "thin";
              CARGO_PROFILE_RELEASE_CODEGEN_UNITS =
                if cargoCgUnits != ""
                then cargoCgUnits
                else "1";
              # `-march=…/-mtune=…` for native CGo/CXX deps; Rust itself
              # reads `-Ctarget-cpu` via `RUSTFLAGS` (already included where set).
              NIX_CFLAGS_COMPILE = mergedCflags;
            };
          in
            if usesEnv
            then {env = old.env // rustEnv;}
            else rustEnv
          else {
            NIX_CFLAGS_COMPILE = appendString (old.NIX_CFLAGS_COMPILE or "") ((resolved.cFlags or []) ++ hardwareFlags);
            NIX_LDFLAGS = appendString (old.NIX_LDFLAGS or "") (resolved.linkFlags or []);
          }
        ));
in {
  inherit
    buildOptimizationProfiles
    buildOptimizationProfileFor
    selectOptimizedPkgs
    applyBuildOptimization
    ;
}
