{lib}: let
  cacheModes = (builtins.fromJSON (builtins.readFile ../data/crossbow-metadata.json)).cacheModes;
in {
  inherit cacheModes;

  cacheModeFor = mode:
    cacheModes.${mode}
    or (throw "crossbow: unsupported cache mode `${mode}`; expected one of ${lib.concatStringsSep ", " (builtins.attrNames cacheModes)}");
}
