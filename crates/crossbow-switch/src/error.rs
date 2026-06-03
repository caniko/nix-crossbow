use std::path::PathBuf;
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

    #[error("crossbow: {flag} requires a value")]
    MissingArgumentValue { flag: String },

    #[error("crossbow: missing {flag}")]
    MissingRequiredArgument { flag: String },

    #[error("crossbow: unsupported nixos-rebuild action `{action}`")]
    UnsupportedAction { action: String },

    #[error(
        "crossbow: --capture requires --publish-command; provide the cache publication command or use --no-capture"
    )]
    CaptureRequiresPublishCommand,

    #[error("crossbow: no publisher configured")]
    NoPublisherConfigured,

    #[error("spawning command `{command}`")]
    CommandSpawn {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("command `{command}` did not open stdin")]
    CommandStdinUnavailable { command: String },

    #[error("writing store paths to command `{command}`")]
    CommandWriteStdin {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("waiting for command `{command}`")]
    CommandWait {
        command: String,
        #[source]
        source: std::io::Error,
    },

    #[error("command `{command}` failed: {stderr}")]
    CommandFailed { command: String, stderr: String },

    #[error("command `{command}` produced non-UTF-8 output")]
    CommandUtf8 {
        command: String,
        #[source]
        source: FromUtf8Error,
    },

    #[error("running nix build for {attr}")]
    NixBuildRun {
        attr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("nix build failed for {attr}: {stderr}")]
    NixBuildFailed { attr: String, stderr: String },

    #[error("nix build produced non-UTF-8 output for {attr}")]
    NixBuildUtf8 {
        attr: String,
        #[source]
        source: FromUtf8Error,
    },

    #[error("nix build printed no output path for {attr}")]
    NixBuildNoOutput { attr: String },

    #[error("running nix-store")]
    NixStoreRun {
        #[source]
        source: std::io::Error,
    },

    #[error("nix-store {args} failed: {stderr}")]
    NixStoreFailed { args: String, stderr: String },

    #[error("nix-store produced non-UTF-8 output")]
    NixStoreUtf8 {
        #[source]
        source: FromUtf8Error,
    },

    #[error("nix-store returned no deriver for {toplevel}")]
    NixStoreNoDeriver { toplevel: PathBuf },

    #[error("running nixos-rebuild {action} for {flake_attr}")]
    NixosRebuildRun {
        action: String,
        flake_attr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("nixos-rebuild {action} failed for {flake_attr}: {stderr}")]
    NixosRebuildFailed {
        action: String,
        flake_attr: String,
        stderr: String,
    },

    #[error("{count} paths missing from cache after publish: {preview}")]
    MissingCachePaths { count: usize, preview: String },

    #[error("crossbow metadata is invalid")]
    MetadataJson {
        #[from]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
