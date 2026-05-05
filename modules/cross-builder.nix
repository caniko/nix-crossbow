{
  config,
  lib,
  ...
}: let
  cfg = config.services.crossbow.crossBuilder;
  inherit (lib) concatStringsSep mapAttrsToList mkIf mkOption types;

  remoteBuilderModule = types.submodule ({name, ...}: {
    options = {
      hostName = mkOption {
        type = types.str;
        default = name;
        description = "SSH alias used by Nix for this remote builder.";
      };
      sshHostName = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Concrete SSH HostName for the generated SSH config.";
      };
      sshPort = mkOption {
        type = types.nullOr types.port;
        default = null;
        description = "Optional SSH port for the generated SSH config.";
      };
      sshUser = mkOption {
        type = types.nullOr types.str;
        default = "root";
        description = "Remote SSH user used by Nix.";
      };
      sshKey = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Local private key path used by Nix for this builder.";
      };
      systems = mkOption {
        type = types.listOf types.str;
        description = "Host systems this builder can execute natively.";
      };
      maxJobs = mkOption {
        type = types.int;
        default = 1;
        description = "Maximum concurrent jobs scheduled on this builder.";
      };
      speedFactor = mkOption {
        type = types.int;
        default = 1;
        description = "Relative builder speed for Nix scheduling.";
      };
      supportedFeatures = mkOption {
        type = types.listOf types.str;
        default = [
          "big-parallel"
          "benchmark"
          "nixos-test"
        ];
        description = "Nix system features supported by this builder.";
      };
      mandatoryFeatures = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Features required for derivations to use this builder.";
      };
      hostKeyAlias = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Optional SSH HostKeyAlias for known_hosts reuse.";
      };
      extraSshConfig = mkOption {
        type = types.lines;
        default = "";
        description = "Extra OpenSSH config lines for this builder host.";
      };
    };
  });

  remoteBuilders = mapAttrsToList (_: builder: builder) cfg.remoteBuilders;

  sshConfigFor = name: builder: ''
    Host ${builder.hostName}
      ${lib.optionalString (builder.sshHostName != null) "HostName ${builder.sshHostName}"}
      ${lib.optionalString (builder.sshPort != null) "Port ${toString builder.sshPort}"}
      ${lib.optionalString (builder.sshUser != null) "User ${builder.sshUser}"}
      ${lib.optionalString (builder.sshKey != null) "IdentityFile ${builder.sshKey}"}
      ${lib.optionalString (builder.hostKeyAlias != null) "HostKeyAlias ${builder.hostKeyAlias}"}
      BatchMode yes
      IdentitiesOnly yes
      ${builder.extraSshConfig}
  '';
in {
  options.services.crossbow.crossBuilder = {
    enable = lib.mkEnableOption "Crossbow remote builder settings";
    systems = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Extra systems this machine can build natively for Crossbow checks.";
    };
    remoteBuilders = mkOption {
      type = types.attrsOf remoteBuilderModule;
      default = {};
      description = "Native remote builders used for Crossbow remote-native/cache-miss builds.";
    };
  };

  config = mkIf cfg.enable {
    nix.settings.extra-platforms = cfg.systems;
    nix.settings.builders-use-substitutes = true;
    nix.distributedBuilds = remoteBuilders != [];
    nix.buildMachines =
      map (builder: {
        inherit
          (builder)
          hostName
          systems
          sshUser
          sshKey
          maxJobs
          speedFactor
          supportedFeatures
          mandatoryFeatures
          ;
      })
      remoteBuilders;

    environment.etc."ssh/ssh_config.d/50-crossbow-builders.conf" = mkIf (cfg.remoteBuilders != {}) {
      text = concatStringsSep "\n" (mapAttrsToList sshConfigFor cfg.remoteBuilders);
    };
  };
}
