{
  config,
  lib,
  ...
}: let
  cfg = config.services.crossbow.crossBuilder;
in {
  options.services.crossbow.crossBuilder = {
    enable = lib.mkEnableOption "Crossbow remote builder settings";
    systems = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Extra systems this machine can build natively for Crossbow checks.";
    };
  };

  config = lib.mkIf cfg.enable {
    nix.settings.extra-platforms = cfg.systems;
  };
}
