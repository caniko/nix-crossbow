{
  pkgs,
  stdenv,
  toolchain,
  hostSystem,
  ...
}:
stdenv.mkDerivation {
  pname = "crossbow-tiny-c";
  version = "0.1.0";

  dontUnpack = true;
  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    cat > tiny.c <<'EOF'
    int main(void) { return 0; }
    EOF
    ${toolchain.CC} tiny.c -o tiny
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out/bin
    cp tiny $out/bin/tiny
    printf '%s\n' '${hostSystem}' > $out/crossbow-host-system
    printf '%s\n' '${toolchain.zigTarget}' > $out/crossbow-zig-target
    runHook postInstall
  '';

  passthru.crossbow.host = hostSystem;
}
