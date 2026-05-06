{
  self,
  isLinux,
  isDarwin,
  isWindows,
  isWasm,
}: rec {
  unsupportedToolchainMessage = {
    build,
    host,
    darwinSdk ? null,
  }:
    if isWasm host
    then "crossbow: wasm toolchain is declared but not implemented in phase 1"
    else if isWindows host
    then "crossbow: windows toolchain is declared but not implemented in phase 1"
    else if isLinux build && isDarwin host && darwinSdk == null
    then ''
      crossbow: linux-to-darwin cross-compilation requires darwinSdk.
      Set darwinSdk = /path/to/MacOSX.sdk and configure osxcross.
    ''
    else if isDarwin host
    then "crossbow: darwin toolchain is declared but not implemented in phase 1"
    else "crossbow: unsupported toolchain pair `${build}` -> `${host}`";

  selectToolchain = {
    build,
    host,
    pkgs,
    darwinSdk ? null,
  }:
    if isLinux build && isLinux host
    then
      import ../toolchains/linux-linux.nix {
        inherit pkgs;
        crossbow = self;
        buildSystem = build;
        hostSystem = host;
      }
    else throw (unsupportedToolchainMessage {inherit build host darwinSdk;});
}
