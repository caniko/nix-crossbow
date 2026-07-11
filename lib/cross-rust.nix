{
  lib,
  self,
}: let
  cargoTargetEnvFor = system:
    lib.toUpper (builtins.replaceStrings ["-"] ["_"] (self.nixSystemToGnuConfig system));
in {
  withCrossRust = {
    crossbowCrossPkgs ? null,
    package,
    extraEnv ? {},
    extraNativeBuildInputs ? [],
    applyCargoArtifacts ? true,
  }:
    if crossbowCrossPkgs == null
    then package
    else let
      buildSystem = crossbowCrossPkgs.stdenv.buildPlatform.system;
      buildCc = crossbowCrossPkgs.buildPackages.stdenv.cc;
      hostCc = "${buildCc}/bin/cc";
      hostLinkerEnv = {
        "CARGO_TARGET_${cargoTargetEnvFor buildSystem}_LINKER" = hostCc;
        HOST_CC = hostCc;
        CC_FOR_BUILD = hostCc;
      };
      crossRustAttrs = old: {
        nativeBuildInputs = (old.nativeBuildInputs or []) ++ extraNativeBuildInputs;
        env = (old.env or {}) // hostLinkerEnv // extraEnv;
      };
    in
      package.overrideAttrs (
        old: let
          attrs = crossRustAttrs old;
        in
          attrs
          // (
            if applyCargoArtifacts && (old ? cargoArtifacts) && (builtins.hasAttr "overrideAttrs" old.cargoArtifacts)
            then {
              cargoArtifacts = old.cargoArtifacts.overrideAttrs crossRustAttrs;
            }
            else {}
          )
      );
}
