{lib}: let
  zigTargets = {
    x86_64-linux = "x86_64-linux-gnu";
    aarch64-linux = "aarch64-linux-gnu";
    armv7l-linux = "arm-linux-gnueabihf";
    riscv64-linux = "riscv64-linux-gnu";
    powerpc64le-linux = "powerpc64le-linux-gnu";
    s390x-linux = "s390x-linux-gnu";
    x86_64-darwin = "x86_64-macos-none";
    aarch64-darwin = "aarch64-macos-none";
    x86_64-windows = "x86_64-windows-gnu";
    aarch64-windows = "aarch64-windows-gnu";
    wasm32-wasi = "wasm32-wasi-musl";
  };

  gnuConfigs = {
    x86_64-linux = "x86_64-unknown-linux-gnu";
    aarch64-linux = "aarch64-unknown-linux-gnu";
    armv7l-linux = "armv7l-unknown-linux-gnueabihf";
    riscv64-linux = "riscv64-unknown-linux-gnu";
    powerpc64le-linux = "powerpc64le-unknown-linux-gnu";
    s390x-linux = "s390x-unknown-linux-gnu";
    x86_64-darwin = "x86_64-apple-darwin";
    aarch64-darwin = "aarch64-apple-darwin";
    x86_64-windows = "x86_64-pc-windows-gnu";
    aarch64-windows = "aarch64-pc-windows-gnu";
    wasm32-wasi = "wasm32-unknown-wasi";
  };

  lookup = name: table: system:
    table.${system}
    or (throw "crossbow: no ${name} mapping for `${system}`");
in {
  inherit zigTargets gnuConfigs;
  nixSystemToZigTarget = lookup "Zig target" zigTargets;
  nixSystemToGnuConfig = lookup "GNU config" gnuConfigs;
}
