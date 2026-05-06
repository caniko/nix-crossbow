{
  lib,
  targets,
}: let
  osOf = system: let
    parts = lib.splitString "-" system;
  in
    lib.last parts;
in {
  inherit osOf;

  isLinux = system: osOf system == "linux";
  isDarwin = system: osOf system == "darwin";
  isWindows = system: osOf system == "windows";
  isWasm = system: lib.hasPrefix "wasm" system;

  targetFor = system:
    targets.${system}
    or (throw "crossbow: unsupported host platform `${system}`; add it to lib/targets.nix and lib/platform-map.nix first");
}
