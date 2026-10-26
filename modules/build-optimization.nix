crossbowLib: {lib, ...}: {
  options.services.crossbow.buildOptimization = {
    enable = lib.mkEnableOption "Crossbow optimized local builds for explicitly wrapped cache-miss derivations";

    profile = lib.mkOption {
      type = lib.types.raw;
      default = crossbowLib.buildOptimizationProfiles.cache-first;
      description = "Crossbow build optimization profile to apply to explicitly wrapped local-build derivations.";
    };

    hardwareProfile = lib.mkOption {
      type = lib.types.nullOr lib.types.raw;
      default = null;
      description = "Optional Crossbow hardware profile for explicitly wrapped local-build derivations.";
    };
  };
}
