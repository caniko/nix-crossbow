use std::string::FromUtf8Error;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error(
        "crossbow: unsupported host platform `{system}`; add it to lib/targets.nix and lib/platform-map.nix first"
    )]
    UnsupportedHostPlatform { system: String },

    #[error("crossbow: no {name} mapping for `{system}`")]
    MissingPlatformMapping { name: String, system: String },

    #[error("crossbow: unsupported cache mode `{mode}`; expected one of {expected}")]
    UnsupportedCacheMode { mode: String, expected: String },

    #[error(
        "crossbow: unsupported hardware optimization profile `{profile}`; expected one of {expected}"
    )]
    UnsupportedHardwareProfile { profile: String, expected: String },

    #[error(
        "crossbow: hardware optimization profile `{profile}` is for `{profile_system}` but host is `{host}`"
    )]
    HardwareProfileHostMismatch {
        profile: String,
        profile_system: String,
        host: String,
    },

    #[error(
        "crossbow: unsupported build optimization profile `{profile}`; expected one of {expected}"
    )]
    UnsupportedBuildOptimizationProfile { profile: String, expected: String },

    #[error("crossbow: native-builder check `{check_name}` requires a non-empty host system")]
    NativeBuilderMissingHost { check_name: String },

    #[error("crossbow: executor `{kind}` is declared but not implemented in phase 1")]
    ExecutorNotImplemented { kind: String },

    #[error("crossbow: unknown executor `{kind}`")]
    UnknownExecutor { kind: String },

    #[error("crossbow: unknown argument `{argument}`\n{usage}")]
    UnknownArgument { argument: String, usage: String },

    #[error("crossbow: {flag} requires a value; pass it as `{flag} <value>`")]
    MissingArgumentValue { flag: String },

    #[error("crossbow: missing {flag}; pass `{flag} <value>`")]
    MissingRequiredArgument { flag: String },

    #[error("crossbow: unsupported nixos-rebuild action `{action}`")]
    UnsupportedAction { action: String },

    #[error(
        "crossbow: --capture requires --publish-command; provide the cache publication command or use --no-capture"
    )]
    CaptureRequiresPublishCommand,

    #[error("crossbow: no publisher configured; pass --publish-command when capture is enabled")]
    NoPublisherConfigured,

    #[error("crossbow: failed to spawn shell command `{command}`")]
    CommandSpawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("crossbow: shell command `{command}` did not open stdin for store paths")]
    CommandStdinUnavailable { command: String },

    #[error("crossbow: failed to write store paths to shell command `{command}`")]
    CommandWriteStdin {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("crossbow: failed to wait for shell command `{command}`")]
    CommandWait {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("crossbow: shell command `{command}` failed: {stderr}")]
    CommandFailed { command: String, stderr: String },

    #[error("crossbow: shell command `{command}` produced non-UTF-8 output")]
    CommandUtf8 {
        command: String,
        #[source]
        source: FromUtf8Error,
    },

    #[error("crossbow: failed to run `nix build --no-link --print-out-paths {attr}`")]
    NixBuildRun {
        attr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("crossbow: `nix build --no-link --print-out-paths {attr}` failed: {stderr}")]
    NixBuildFailed { attr: String, stderr: String },

    #[error("crossbow: `nix build --no-link --print-out-paths {attr}` produced non-UTF-8 output")]
    NixBuildUtf8 {
        attr: String,
        #[source]
        source: FromUtf8Error,
    },

    #[error("crossbow: `nix build --no-link --print-out-paths {attr}` printed no output path")]
    NixBuildNoOutput { attr: String },

    #[error("crossbow: failed to run `nix-store {args}`")]
    NixStoreRun {
        args: String,
        #[source]
        source: std::io::Error,
    },

    #[error("crossbow: `nix-store {args}` failed: {stderr}")]
    NixStoreFailed { args: String, stderr: String },

    #[error("crossbow: `nix-store {args}` produced non-UTF-8 output")]
    NixStoreUtf8 {
        args: String,
        #[source]
        source: FromUtf8Error,
    },

    #[error("crossbow: `nix-store --query --deriver {toplevel}` returned no deriver")]
    NixStoreNoDeriver { toplevel: String },

    #[error("crossbow: failed to run `nixos-rebuild {action} --flake {flake_attr}`")]
    NixosRebuildRun {
        action: String,
        flake_attr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("crossbow: `nixos-rebuild {action} --flake {flake_attr}` failed: {stderr}")]
    NixosRebuildFailed {
        action: String,
        flake_attr: String,
        stderr: String,
    },

    #[error(
        "crossbow: {count} paths are missing from cache after publish; first missing paths: {preview}"
    )]
    MissingCachePaths { count: usize, preview: String },

    #[error("crossbow: embedded metadata JSON is invalid")]
    MetadataJson {
        #[from]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
