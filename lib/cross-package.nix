{
  lib,
  self,
  targetFor,
  selectToolchain,
}: rec {
  mkCross = {
    pkgs,
    host,
    package,
    build ? pkgs.stdenv.buildPlatform.system,
    target ? null,
    toolchain ?
      selectToolchain {
        inherit build host pkgs;
      },
    doCheck ? true,
    checkRunner ? null,
    darwinSdk ? null,
  }: let
    hostTarget = targetFor host;
    invokePackage = fn:
      fn {
        inherit pkgs toolchain;
        buildSystem = build;
        hostSystem = host;
        targetSystem = target;
        stdenv = pkgs.stdenv;
      };
    out =
      if lib.isFunction package
      then invokePackage package
      else invokePackage (import package);
    check =
      if doCheck
      then
        mkCrossCheck {
          inherit pkgs out;
          host = hostTarget;
          executor =
            if checkRunner != null
            then checkRunner
            else self.executors.native-builder {};
        }
      else null;
  in
    out
    // {
      passthru =
        (out.passthru or {})
        // {
          crossbow = {
            inherit build host target toolchain check;
          };
        };
    };

  mkCrossCheck = {
    pkgs,
    out,
    host,
    executor ? self.executors.native-builder {},
  }: let
    checkName = out.name or out.pname or "crossbow";
  in
    if executor.kind == "skip"
    then
      pkgs.runCommand "${checkName}-check-skipped" {
        passthru.crossbow.skipReason = executor.reason;
      } ''
        printf '%s\n' '${executor.reason}' > $out
      ''
    else if executor.kind == "native-builder"
    then
      pkgs.runCommand "${checkName}-native-builder-check" {
        passthru.crossbow.requiredSystem = host.system;
      } ''
        test -n '${host.system}'
        touch $out
      ''
    else throw "crossbow: executor `${executor.kind}` is not implemented in phase 1";
}
