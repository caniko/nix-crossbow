use std::{fmt, string::FromUtf8Error};

/// Maximum diagnostic size retained for a failed realization.
pub const MAX_REALIZATION_DIAGNOSTIC_BYTES: usize = 4096;

/// The bounded class of prerequisite or realization failure reported by Nix.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealizationFailureKind {
    /// A required output was not available from the configured substituters.
    MissingCacheRoot,
    /// A required output belongs to a platform unavailable on the build host.
    WrongPlatform,
    /// An explicitly selected remote builder failed or could not be reached.
    RemoteBuilder,
    /// Nix rejected or failed the realization without a more specific class.
    NixRealization,
    /// The failure did not match a known Nix diagnostic.
    Unknown,
}

impl fmt::Display for RealizationFailureKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingCacheRoot => "missing-cache-root",
            Self::WrongPlatform => "wrong-platform",
            Self::RemoteBuilder => "remote-builder",
            Self::NixRealization => "nix-realization",
            Self::Unknown => "unknown",
        })
    }
}

/// Bounded, redacted context for a failed realization prerequisite.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MissingPrerequisite {
    /// Failed requirement or realization root.
    pub root: String,
    /// Affected Nix system, or `unknown` when Nix did not identify one.
    pub system: String,
    /// Non-sensitive cache or builder context.
    pub cache_context: String,
    /// Whether explicit native recovery may satisfy this prerequisite.
    pub recoverable_by_native_recovery: bool,
    /// Bounded, redacted diagnostic preview.
    pub diagnostic: String,
    /// Stable failure classification.
    pub kind: RealizationFailureKind,
}

impl fmt::Display for MissingPrerequisite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} root={} system={} context={} recoverable_by_native_recovery={}: {}",
            self.kind,
            self.root,
            self.system,
            self.cache_context,
            self.recoverable_by_native_recovery,
            self.diagnostic
        )
    }
}

/// Errors produced by Crossbow switch planning, metadata lookup, and runtime commands.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A target system was not present in the shared metadata.
    #[error(
        "crossbow: unsupported host platform `{system}`; add it to lib/targets.nix and lib/platform-map.nix first"
    )]
    UnsupportedHostPlatform {
        /// Unsupported Nix system string.
        system: String,
    },

    /// A Nix system did not have the requested compiler target mapping.
    #[error("crossbow: no {name} mapping for `{system}`")]
    MissingPlatformMapping {
        /// Mapping table name.
        name: String,
        /// Nix system string that was missing.
        system: String,
    },

    /// A cache mode name was not present in the shared metadata.
    #[error("crossbow: unsupported cache mode `{mode}`; expected one of {expected}")]
    UnsupportedCacheMode {
        /// Unsupported cache mode.
        mode: String,
        /// Comma-separated supported cache modes.
        expected: String,
    },

    /// A hardware optimization profile name was not present in the shared metadata.
    #[error(
        "crossbow: unsupported hardware optimization profile `{profile}`; expected one of {expected}"
    )]
    UnsupportedHardwareProfile {
        /// Unsupported profile name.
        profile: String,
        /// Comma-separated supported profile names.
        expected: String,
    },

    /// A hardware optimization profile was used with the wrong host system.
    #[error(
        "crossbow: hardware optimization profile `{profile}` is for `{profile_system}` but host is `{host}`"
    )]
    HardwareProfileHostMismatch {
        /// Profile name.
        profile: String,
        /// System the profile supports.
        profile_system: String,
        /// Host system requested by the caller.
        host: String,
    },

    /// A build optimization profile name was not present in the shared metadata.
    #[error(
        "crossbow: unsupported build optimization profile `{profile}`; expected one of {expected}"
    )]
    UnsupportedBuildOptimizationProfile {
        /// Unsupported profile name.
        profile: String,
        /// Comma-separated supported profile names.
        expected: String,
    },

    /// A native-builder check was planned without a host system.
    #[error("crossbow: native-builder check `{check_name}` requires a non-empty host system")]
    NativeBuilderMissingHost {
        /// Check derivation name.
        check_name: String,
    },

    /// A declared executor exists in metadata but has no phase-1 implementation.
    #[error("crossbow: executor `{kind}` is declared but not implemented in phase 1")]
    ExecutorNotImplemented {
        /// Executor kind.
        kind: String,
    },

    /// An executor kind string is not recognized by Crossbow.
    #[error("crossbow: unknown executor `{kind}`")]
    UnknownExecutor {
        /// Unknown executor kind.
        kind: String,
    },

    /// A CLI argument was not recognized.
    #[error("crossbow: unknown argument `{argument}`\n{usage}")]
    UnknownArgument {
        /// Unknown argument.
        argument: String,
        /// Usage text shown to the user.
        usage: String,
    },

    /// A CLI option that requires a value was not followed by one.
    #[error("crossbow: {flag} requires a value; pass it as `{flag} <value>`")]
    MissingArgumentValue {
        /// CLI flag missing its value.
        flag: String,
    },

    /// A required CLI option was absent.
    #[error("crossbow: missing {flag}; pass `{flag} <value>`")]
    MissingRequiredArgument {
        /// Required CLI flag.
        flag: String,
    },

    /// A rebuild action is not supported by the switch CLI.
    #[error("crossbow: unsupported nixos-rebuild action `{action}`")]
    UnsupportedAction {
        /// Unsupported rebuild action.
        action: String,
    },

    /// An invalid value was passed to `--max-jobs`.
    #[error("crossbow: invalid --max-jobs value `{value}`; expected a positive integer")]
    InvalidMaxJobs {
        /// Invalid value passed to --max-jobs.
        value: String,
    },

    /// A remote-native realization was requested without a builder.
    #[error("crossbow: remote-native realization requires at least one explicit builder")]
    RemoteNativeRequiresBuilder,

    /// An unknown realization policy name was supplied.
    #[error(
        "crossbow: invalid realization policy `{value}`; expected substitute-only or remote-native"
    )]
    InvalidRealizationPolicy {
        /// Policy name supplied by the caller.
        value: String,
    },

    /// A machine-readable planner command could not be spawned.
    #[error("crossbow: failed to run planner command `{command}`")]
    PlannerCommand {
        /// Command text.
        command: String,
        /// Underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// A machine-readable planner command failed.
    #[error("crossbow: planner command `{command}` failed: {stderr}")]
    PlannerCommandFailed {
        /// Command text.
        command: String,
        /// Captured stderr.
        stderr: String,
    },

    /// A planner command emitted invalid UTF-8.
    #[error("crossbow: planner command `{command}` emitted non-UTF-8 output")]
    PlannerUtf8 {
        /// Command text.
        command: String,
        /// Underlying UTF-8 error.
        #[source]
        source: FromUtf8Error,
    },

    /// Nix planner JSON was invalid.
    #[error("crossbow: planner JSON is invalid")]
    PlannerJson {
        /// JSON parsing failure.
        #[source]
        source: serde_json::Error,
    },

    /// Nix omitted one requested derivation from `nix derivation show`.
    #[error("crossbow: planner could not find derivation `{drv}` in nix derivation show output")]
    PlannerMissingDerivation {
        /// Missing derivation path.
        drv: String,
    },

    /// Nix omitted one requested output while resolving a derivation output.
    #[error("crossbow: planner could not resolve output `{output}` of `{drv}`")]
    PlannerMissingOutput {
        /// Derivation path.
        drv: String,
        /// Output name.
        output: String,
    },

    /// The exact planner was invoked without any cache to probe.
    #[error("crossbow: exact planner requires at least one configured substituter")]
    PlannerNoSubstituters,

    /// An executable plan uses an unsupported schema version.
    #[error("crossbow: unsupported closure plan schema version {version}")]
    UnsupportedPlanSchema {
        /// Unsupported schema version.
        version: u32,
    },

    /// An executable plan's content no longer matches its stable identifier.
    #[error("crossbow: closure plan ID mismatch: expected `{expected}`, computed `{actual}`")]
    PlanIdMismatch {
        /// Identifier recorded in the plan.
        expected: String,
        /// Identifier computed from canonical content.
        actual: String,
    },

    /// A caller attempted to execute a plan for another installable identity.
    #[error(
        "crossbow: closure plan identity mismatch for `{field}`: planned `{planned}`, requested `{requested}`"
    )]
    PlanIdentityMismatch {
        /// Identity field that differed.
        field: &'static str,
        /// Identity sealed into the plan.
        planned: String,
        /// Identity supplied by the executor caller.
        requested: String,
    },

    /// A fail-closed output action prevents execution.
    #[error("crossbow: closure plan is blocked for `{path}`: {reason}")]
    PlanBlocked {
        /// Output path that cannot be realized safely.
        path: String,
        /// Planner reason for refusing execution.
        reason: String,
    },

    /// A cache probe could not be spawned.
    #[error("crossbow: failed to probe `{output}` in substituter `{substituter}`")]
    PlannerCacheProbe {
        /// Substituter store URL.
        substituter: String,
        /// Output path being queried.
        output: String,
        /// Underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// A cache probe failed without returning a per-path JSON answer.
    #[error("crossbow: cache probe for `{output}` in `{substituter}` failed: {stderr}")]
    PlannerCacheProbeFailed {
        /// Substituter store URL.
        substituter: String,
        /// Output path being queried.
        output: String,
        /// Captured stderr.
        stderr: String,
    },

    /// A cache probe returned malformed or incomplete JSON.
    #[error("crossbow: cache probe JSON for `{output}` in `{substituter}` is invalid: {reason}")]
    PlannerCacheProbeJson {
        /// Substituter store URL.
        substituter: String,
        /// Output path being queried.
        output: String,
        /// Why the response could not be used.
        reason: String,
    },

    /// Capture mode was requested without a publication command.
    #[error(
        "crossbow: --capture requires --publish-command; provide the cache publication command or use --no-capture"
    )]
    CaptureRequiresPublishCommand,

    /// Runtime switch orchestration reached publish without a publisher.
    #[error("crossbow: no publisher configured; pass --publish-command when capture is enabled")]
    NoPublisherConfigured,

    /// A shell command used for publish/verify could not be spawned.
    #[error("crossbow: failed to spawn shell command `{command}`")]
    CommandSpawn {
        /// Shell command text.
        command: String,
        /// Underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// A shell command did not expose stdin for store paths.
    #[error("crossbow: shell command `{command}` did not open stdin for store paths")]
    CommandStdinUnavailable {
        /// Shell command text.
        command: String,
    },

    /// Writing store paths to a shell command failed.
    #[error("crossbow: failed to write store paths to shell command `{command}`")]
    CommandWriteStdin {
        /// Shell command text.
        command: String,
        /// Underlying write failure.
        #[source]
        source: std::io::Error,
    },

    /// Waiting for a shell command failed.
    #[error("crossbow: failed to wait for shell command `{command}`")]
    CommandWait {
        /// Shell command text.
        command: String,
        /// Underlying wait failure.
        #[source]
        source: std::io::Error,
    },

    /// A shell command exited unsuccessfully.
    #[error("crossbow: shell command `{command}` failed: {stderr}")]
    CommandFailed {
        /// Shell command text.
        command: String,
        /// Captured stderr.
        stderr: String,
    },

    /// A shell command produced stdout that was not UTF-8.
    #[error("crossbow: shell command `{command}` produced non-UTF-8 output")]
    CommandUtf8 {
        /// Shell command text.
        command: String,
        /// UTF-8 decoding failure.
        #[source]
        source: FromUtf8Error,
    },

    /// Spawning `nix build` failed.
    #[error("crossbow: failed to run `nix build --no-link --print-out-paths {attr}`")]
    NixBuildRun {
        /// Nix attribute passed to `nix build`.
        attr: String,
        /// Underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// The initial Crossbow toplevel realization failed.
    #[error("crossbow: realization of `{attr}` failed (exit status {status:?}): {stderr}")]
    RealizationFailed {
        /// Nix attribute passed to `nix build`.
        attr: String,
        /// Process exit code, or `None` when the process ended without one.
        status: Option<i32>,
        /// Captured stderr.
        stderr: String,
    },

    /// The initial realization failed with a structured prerequisite context.
    #[error("crossbow: realization of `{attr}` failed (exit status {status:?}): {prerequisite}")]
    RealizationMissingPrerequisite {
        /// Nix attribute passed to `nix build`.
        attr: String,
        /// Process exit code, or `None` when the process ended without one.
        status: Option<i32>,
        /// Bounded context for the failed prerequisite.
        prerequisite: Box<MissingPrerequisite>,
    },

    /// `nix build` printed stdout that was not UTF-8.
    #[error("crossbow: `nix build --no-link --print-out-paths {attr}` produced non-UTF-8 output")]
    NixBuildUtf8 {
        /// Nix attribute passed to `nix build`.
        attr: String,
        /// UTF-8 decoding failure.
        #[source]
        source: FromUtf8Error,
    },

    /// `nix build --print-out-paths` printed no output path.
    #[error("crossbow: `nix build --no-link --print-out-paths {attr}` printed no output path")]
    NixBuildNoOutput {
        /// Nix attribute passed to `nix build`.
        attr: String,
    },

    /// Spawning `nix-store` failed.
    #[error("crossbow: failed to run `nix-store {args}`")]
    NixStoreRun {
        /// Arguments passed to `nix-store`.
        args: String,
        /// Underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// `nix-store` exited unsuccessfully.
    #[error("crossbow: `nix-store {args}` failed: {stderr}")]
    NixStoreFailed {
        /// Arguments passed to `nix-store`.
        args: String,
        /// Captured stderr.
        stderr: String,
    },

    /// `nix-store` printed stdout that was not UTF-8.
    #[error("crossbow: `nix-store {args}` produced non-UTF-8 output")]
    NixStoreUtf8 {
        /// Arguments passed to `nix-store`.
        args: String,
        /// UTF-8 decoding failure.
        #[source]
        source: FromUtf8Error,
    },

    /// `nix-store --query --deriver` returned no deriver path.
    #[error("crossbow: `nix-store --query --deriver {toplevel}` returned no deriver")]
    NixStoreNoDeriver {
        /// Toplevel store path queried for its deriver.
        toplevel: String,
    },

    /// Reading a Nix store file (e.g. from a `crossbowRequirements` artifact) failed.
    #[error("crossbow: failed to read `{file}`")]
    NixStoreRead {
        /// File path that failed to read.
        file: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Spawning `nixos-rebuild` failed.
    #[error("crossbow: failed to run `nixos-rebuild {action} --flake {flake_attr}`")]
    NixosRebuildRun {
        /// Rebuild action.
        action: String,
        /// Flake reference passed to `nixos-rebuild`.
        flake_attr: String,
        /// Underlying spawn failure.
        #[source]
        source: std::io::Error,
    },

    /// `nixos-rebuild` exited unsuccessfully.
    #[error("crossbow: `nixos-rebuild {action} --flake {flake_attr}` failed: {stderr}")]
    NixosRebuildFailed {
        /// Rebuild action.
        action: String,
        /// Flake reference passed to `nixos-rebuild`.
        flake_attr: String,
        /// Captured stderr.
        stderr: String,
    },

    /// Cache verification reported paths missing after publication.
    #[error(
        "crossbow: {count} paths are missing from cache after publish; first missing paths: {preview}"
    )]
    MissingCachePaths {
        /// Number of missing paths.
        count: usize,
        /// Preview of missing paths.
        preview: String,
    },

    /// Embedded JSON metadata failed to deserialize.
    #[error("crossbow: embedded metadata JSON is invalid")]
    MetadataJson {
        /// JSON parsing failure.
        #[from]
        source: serde_json::Error,
    },
}

/// Convenient result alias for Crossbow switch operations.
pub type Result<T> = std::result::Result<T, Error>;
