{
  description = "QEMU-free cross-compilation helpers for Nix flakes";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];

      flake = {
        lib = import ./lib {inherit inputs;};
        nixosModules.cross-builder = import ./modules/cross-builder.nix;
      };

      perSystem = {
        pkgs,
        system,
        ...
      }: let
        formatter = pkgs.writeShellApplication {
          name = "crossbow-fmt";
          runtimeInputs = [pkgs.alejandra];
          text = ''
            if [ "$#" -eq 0 ]; then
              set -- .
            fi

            if [ "$#" -eq 1 ] && [ "$1" = "--check" ]; then
              set -- --check .
            fi

            exec alejandra "$@"
          '';
        };
      in {
        inherit formatter;

        packages.tiny-c-aarch64-linux = inputs.self.lib.mkCross {
          inherit pkgs;
          build = system;
          host = "aarch64-linux";
          package = import ./examples/tiny-c.nix;
          doCheck = false;
        };

        checks = {
          platform-map = pkgs.runCommand "crossbow-platform-map" {} ''
            test "${inputs.self.lib.nixSystemToZigTarget "aarch64-linux"}" = "aarch64-linux-gnu"
            test "${inputs.self.lib.nixSystemToGnuConfig "aarch64-linux"}" = "aarch64-unknown-linux-gnu"
            test "${inputs.self.lib.targets.wasm32-wasi.config}" = "wasm32-wasi"
            test "${inputs.self.lib.cacheModes.strict-cross.name}" = "strict-cross"
            test "${inputs.self.lib.cacheModes.native-substituted.name}" = "native-substituted"
            test "${inputs.self.lib.hardwareProfiles.rockpro64.platform.gcc.tune}" = "cortex-a72.cortex-a53"
            test "${inputs.self.lib.hardwareOptimizationName "rockpro64"}" = "rockpro64"
            touch $out
          '';

          tiny-c-aarch64-linux = inputs.self.packages.${system}.tiny-c-aarch64-linux;

          unsupported-linux-darwin-message = pkgs.runCommand "crossbow-unsupported-linux-darwin-message" {} ''
            message='${builtins.replaceStrings ["'"] ["'\\''"] (inputs.self.lib.unsupportedToolchainMessage {
              build = "x86_64-linux";
              host = "aarch64-darwin";
            })}'
            printf '%s' "$message" | grep -F "darwinSdk" >/dev/null
            touch $out
          '';
        };

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.alejandra
            pkgs.nix
            pkgs.zig
          ];
        };
      };
    };
}
