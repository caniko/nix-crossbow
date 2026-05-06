{lib}: let
  cacheModes = {
    strict-cross = {
      name = "strict-cross";
      proves = "build machine compiles host artifacts without QEMU";
      cacheExpectation = "low nixpkgs binary cache reuse because cross derivation paths differ from native paths";
    };
    native-substituted = {
      name = "native-substituted";
      proves = "build machine can orchestrate and substitute host-system artifacts without QEMU";
      cacheExpectation = "high nixpkgs binary cache reuse when host-system paths are available from substituters";
    };
    remote-native = {
      name = "remote-native";
      proves = "build machine can route missing host-system builds to native hardware without QEMU";
      cacheExpectation = "uses substitutes first, then a native remote builder for missing host-system paths";
    };
    cache-shaped-with-cross-overrides = {
      name = "cache-shaped-with-cross-overrides";
      proves = "host-system closure keeps native cache shape while explicit packages may be cross-built";
      cacheExpectation = "high nixpkgs binary cache reuse; only opt-in package overrides use cross derivation paths";
    };
  };
in {
  inherit cacheModes;

  cacheModeFor = mode:
    cacheModes.${mode}
    or (throw "crossbow: unsupported cache mode `${mode}`; expected one of ${lib.concatStringsSep ", " (builtins.attrNames cacheModes)}");
}
