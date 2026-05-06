{
  lib,
  mkCross,
}: {
  withCrossSupport = {
    inputs,
    buildSystems,
    crossTargets,
    packages,
    darwinSdk ? null,
  }: let
    forBuildSystem = build: let
      pkgs = inputs.nixpkgs.legacyPackages.${build};
      native = packages pkgs;
      crossForPackage = name: package:
        lib.listToAttrs (map (target: {
            name = "${name}-${target.system}";
            value = mkCross {
              inherit pkgs build package darwinSdk;
              host = target.system;
              doCheck = false;
            };
          })
          crossTargets);
    in
      native // lib.concatMapAttrs crossForPackage native;
  in {
    packages = lib.genAttrs buildSystems forBuildSystem;
  };
}
