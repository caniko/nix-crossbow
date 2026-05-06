# Mirror of nixos/modules/system/activation/top-level.nix's `baseSystem`
# construction, but built via `buildPkgs.stdenvNoCC.mkDerivation` so the
# resulting derivation has `system = build`. The script is intentionally
# identical to nixpkgs upstream — it is shell that writes symlinks and
# text files, identical content regardless of build platform — and the
# references inside it (kernel, systemd, etc) remain native host paths
# that substitute from cache.
{}: {
  mkToplevelOverrideModule = {buildPkgs}: {
    config,
    lib,
    ...
  }: let
    inherit (lib) optionalString;
    systemBuilder = ''
      mkdir $out

      ${
        if config.boot.initrd.enable && config.boot.initrd.systemd.enable
        then ''
          cp "$systemd/lib/systemd/systemd" $out/init

          ${optionalString (!config.system.nixos-init.enable) ''
            cp ${config.system.build.bootStage2} $out/prepare-root
            substituteInPlace $out/prepare-root --subst-var-by systemConfig $out
          ''}
        ''
        else ''
          cp ${config.system.build.bootStage2} $out/init
          substituteInPlace $out/init --subst-var-by systemConfig $out
        ''
      }

      ln -s ${config.system.build.etc}/etc $out/etc

      ln -s ${config.system.path} $out/sw
      ln -s "$systemd" $out/systemd

      echo -n "systemd ${toString config.systemd.package.interfaceVersion}" > $out/init-interface-version
      echo -n "$nixosLabel" > $out/nixos-version
      echo -n "${config.boot.kernelPackages.stdenv.hostPlatform.system}" > $out/system

      ${config.system.systemBuilderCommands}

      cp "$extraDependenciesPath" "$out/extra-dependencies"

      ${optionalString (!config.boot.isContainer && config.boot.bootspec.enable) ''
        ${config.boot.bootspec.writer}
        ${optionalString config.boot.bootspec.enableValidation ''${config.boot.bootspec.validator} "$out/${config.boot.bootspec.filename}"''}
      ''}
    '';

    crossbowBaseSystem = buildPkgs.stdenvNoCC.mkDerivation (
      {
        name = "nixos-system-${config.system.name}-${config.system.nixos.label}";
        preferLocalBuild = true;
        allowSubstitutes = false;
        passAsFile = ["extraDependencies"];
        buildCommand = systemBuilder;

        systemd = config.systemd.package;

        nixosLabel = config.system.nixos.label;

        inherit (config.system) extraDependencies;
      }
      // config.system.systemBuilderArgs
    );
  in {
    system.build.toplevel = lib.mkForce (
      lib.asserts.checkAssertWarn config.assertions config.warnings crossbowBaseSystem
    );
  };
}
