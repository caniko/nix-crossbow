{
  lib,
  targetFor,
}: let
  # x86_64 CPU microarchitecture profiles. Each entry corresponds to a
  # `-march=` value that gcc/clang accept on x86_64. Tune defaults to the
  # same value (gcc treats `-mtune` as a follow-on optimization knob) and
  # is omitted unless a more specific tune target is desirable.
  mkX86Profile = arch: description: {
    name = arch;
    system = "x86_64-linux";
    description = "${description} (x86_64 -march=${arch})";
    platform = {
      gcc = {
        inherit arch;
        tune = arch;
      };
    };
  };

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

    # AMD Zen microarchitectures
    znver2 = mkX86Profile "znver2" "AMD Zen 2 (Ryzen 3000 / EPYC Rome)";
    znver3 = mkX86Profile "znver3" "AMD Zen 3 (Ryzen 5000 / EPYC Milan)";
    znver4 = mkX86Profile "znver4" "AMD Zen 4 (Ryzen 7000 / EPYC Genoa)";
    znver5 = mkX86Profile "znver5" "AMD Zen 5 (Ryzen 9000 / EPYC Turin)";

    # Intel microarchitectures (the common server/desktop set)
    skylake = mkX86Profile "skylake" "Intel Skylake (6th-gen Core / Skylake-X)";
    icelake-client = mkX86Profile "icelake-client" "Intel Ice Lake (10th-gen mobile/desktop client)";
    icelake-server = mkX86Profile "icelake-server" "Intel Ice Lake-SP server";
    tigerlake = mkX86Profile "tigerlake" "Intel Tiger Lake (11th-gen mobile)";
    alderlake = mkX86Profile "alderlake" "Intel Alder Lake (12th-gen)";
    raptorlake = mkX86Profile "raptorlake" "Intel Raptor Lake (13th/14th-gen)";
    sapphirerapids = mkX86Profile "sapphirerapids" "Intel Sapphire Rapids server";
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
