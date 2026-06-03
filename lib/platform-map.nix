{lib}: let
  metadata = builtins.fromJSON (builtins.readFile ../data/crossbow-metadata.json);
  inherit (metadata.platformMappings) zigTargets gnuConfigs;

  lookup = name: table: system:
    table.${system}
    or (throw "crossbow: no ${name} mapping for `${system}`");
in {
  inherit zigTargets gnuConfigs;
  nixSystemToZigTarget = lookup "Zig target" zigTargets;
  nixSystemToGnuConfig = lookup "GNU config" gnuConfigs;
}
