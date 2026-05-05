{
  x86_64-linux = {
    system = "x86_64-linux";
    config = "x86_64-unknown-linux-gnu";
  };
  aarch64-linux = {
    system = "aarch64-linux";
    config = "aarch64-unknown-linux-gnu";
  };
  aarch64-musl = {
    system = "aarch64-linux";
    config = "aarch64-unknown-linux-musl";
  };
  armv7l-linux = {
    system = "armv7l-linux";
    config = "armv7l-unknown-linux-gnueabihf";
  };
  riscv64-linux = {
    system = "riscv64-linux";
    config = "riscv64-unknown-linux-gnu";
  };
  powerpc64le = {
    system = "powerpc64le-linux";
    config = "powerpc64le-unknown-linux-gnu";
  };
  s390x = {
    system = "s390x-linux";
    config = "s390x-unknown-linux-gnu";
  };
  aarch64-darwin = {
    system = "aarch64-darwin";
    config = "aarch64-apple-darwin";
  };
  x86_64-darwin = {
    system = "x86_64-darwin";
    config = "x86_64-apple-darwin";
  };
  x86_64-windows = {
    system = "x86_64-windows";
    config = "x86_64-pc-windows-gnu";
  };
  aarch64-windows = {
    system = "aarch64-windows";
    config = "aarch64-pc-windows-gnullvm";
  };
  wasm32-wasi = {
    system = "wasm32-wasi";
    config = "wasm32-wasi";
  };
  wasm32-unknown = {
    system = "wasm32-unknown";
    config = "wasm32-unknown-unknown";
  };
}
