{
  pkgs,
  crossbow,
  buildSystem,
  hostSystem,
}: let
  zigTarget = crossbow.nixSystemToZigTarget hostSystem;
  buildConfig = crossbow.nixSystemToGnuConfig buildSystem;
  hostConfig = crossbow.nixSystemToGnuConfig hostSystem;

  zigCc = pkgs.writeShellScript "crossbow-zig-cc-${hostSystem}" ''
    export ZIG_GLOBAL_CACHE_DIR="''${ZIG_GLOBAL_CACHE_DIR:-''${TMPDIR:-/tmp}/zig-global-cache}"
    export ZIG_LOCAL_CACHE_DIR="''${ZIG_LOCAL_CACHE_DIR:-''${TMPDIR:-/tmp}/zig-local-cache}"
    exec ${pkgs.zig}/bin/zig cc -target ${zigTarget} "$@"
  '';
  zigCxx = pkgs.writeShellScript "crossbow-zig-cxx-${hostSystem}" ''
    export ZIG_GLOBAL_CACHE_DIR="''${ZIG_GLOBAL_CACHE_DIR:-''${TMPDIR:-/tmp}/zig-global-cache}"
    export ZIG_LOCAL_CACHE_DIR="''${ZIG_LOCAL_CACHE_DIR:-''${TMPDIR:-/tmp}/zig-local-cache}"
    exec ${pkgs.zig}/bin/zig c++ -target ${zigTarget} "$@"
  '';
in {
  name = "linux-linux-zig-llvm";
  inherit buildSystem hostSystem zigTarget buildConfig hostConfig;
  CC = zigCc;
  CXX = zigCxx;
  AR = "${pkgs.llvmPackages.bintools}/bin/llvm-ar";
  RANLIB = "${pkgs.llvmPackages.bintools}/bin/llvm-ranlib";
  configureFlags = [
    "--build=${buildConfig}"
    "--host=${hostConfig}"
  ];
}
