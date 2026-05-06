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
in {
  inherit
    buildOptimizationProfiles
    buildOptimizationProfileFor
    applyBuildOptimization
    ;
}
