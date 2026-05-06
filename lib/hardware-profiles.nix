{
  lib,
  targetFor,
}: let
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
in {
  inherit
    hardwareProfiles
    hardwareProfileFor
    mkOptimizedHostPlatform
    hardwareOptimizationName
    ;
}
