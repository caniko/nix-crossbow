//! Runtime support for Crossbow cache-shaped NixOS switch orchestration.
//!
//! The crate keeps impure runtime work in Rust: building a toplevel, collecting
//! the closure to publish, checking cache availability, and finally invoking
//! `nixos-rebuild` with QEMU-free cache-shaped flags.

#![warn(missing_docs)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

/// Command-line entry points for the `crossbow-switch` binary.
pub mod cli;
/// Error and result types shared by the library and binary.
pub mod error;
/// Executor descriptor validation for Crossbow check planning.
pub mod executor;
/// Typed access to Crossbow's shared target/profile metadata.
pub mod metadata;
/// Structured derivation-closure planning for cache-shaped realizations.
pub mod planner;
/// Parsed Crossbow requirements artifact (roots, drvs, fingerprint).
pub mod requirements;
/// Generic prepared-state persistence for prerequisite roots.
pub mod state;

pub use error::{Error, Result};
pub use planner::{plan_closure, ClosurePlan, DerivationClass, DerivationPlan, PlanCounts};
pub use requirements::{diff_roots, label_roots, LabeledRoot, RequirementsArtifact, RootDiff};
pub use state::{
    load_prepared_state, save_prepared_state, state_file_path, state_to_labeled_roots,
    PreparedState, StateStatus,
};

/// Controls which builders may realize missing derivations before activation.
///
/// `SubstituteOnly` is the safe default. `RemoteNative` is explicit opt-in and
/// accepts only the builder specifications supplied by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RealizationPolicy {
    /// Use configured substituters and no builders.
    #[default]
    SubstituteOnly,
    /// Use only these explicitly declared Nix builder specifications.
    RemoteNative {
        /// Nix builder specifications, one per entry.
        builders: Vec<String>,
    },
}

impl RealizationPolicy {
    fn builders_value(&self) -> Result<String> {
        match self {
            Self::SubstituteOnly => Ok(String::new()),
            Self::RemoteNative { builders } if builders.is_empty() => {
                Err(Error::RemoteNativeRequiresBuilder)
            }
            Self::RemoteNative { builders } => Ok(builders.join("\n")),
        }
    }
}

/// Returns the canonical Nix flags for cache-shaped Crossbow realisation.
///
/// The empty `--builders` and `extra-platforms` values keep activation from
/// falling back to remote builders or binfmt when a host-system path is missing
/// from substituters. `always-allow-substitutes` lets the target substitute
/// paths even when individual derivations set `allowSubstitutes = false`.
#[must_use]
pub fn cache_shaped_nix_flags() -> Vec<String> {
    realization_nix_flags(&RealizationPolicy::SubstituteOnly).expect("substitute-only is valid")
}

/// Returns Nix flags for an explicit realization policy.
///
/// All policies keep `extra-platforms` empty. The policy is intentionally not
/// used by activation, which remains substitution-only.
pub fn realization_nix_flags(policy: &RealizationPolicy) -> Result<Vec<String>> {
    Ok(vec![
        "--builders".to_string(),
        policy.builders_value()?,
        "--option".to_string(),
        "extra-platforms".to_string(),
        String::new(),
        "--option".to_string(),
        "always-allow-substitutes".to_string(),
        "true".to_string(),
    ])
}

/// Returns the canonical flags for a cache-shaped crossbow `nixos-rebuild`.
///
/// `--no-reexec` keeps the build-host `nixos-rebuild` process in charge instead
/// of re-executing `config.system.build.nixos-rebuild` from the target flake;
/// the latter is host-platform code in cache-shaped Crossbow configs and would
/// run under binfmt on the build host.
/// `use_substitutes` adds `--use-substitutes`, which shifts transfer to the
/// target's own substituters and is only appropriate when the target trusts the
/// cache that has been populated before activation.
#[must_use]
pub fn cache_shaped_switch_flags(use_substitutes: bool) -> Vec<String> {
    let mut flags = vec!["--no-reexec".to_string()];
    flags.extend(cache_shaped_nix_flags());

    if use_substitutes {
        flags.push("--use-substitutes".to_string());
    }

    flags
}

/// Returns the canonical flags for the initial toplevel realisation.
///
/// These mirror the cache-shaped switch flags that prevent activation from
/// using remote builders or binfmt. The build phase needs the same guard:
/// otherwise a cache miss on a binfmt-enabled build host silently becomes a
/// local emulated build before Crossbow can publish or verify anything.
/// `max_jobs`, when set, is passed as `--max-jobs N` to cap build parallelism
/// on memory-constrained build hosts.
#[must_use]
pub fn cache_shaped_build_args(attr: &str, max_jobs: Option<u32>) -> Vec<String> {
    cache_shaped_build_args_with_policy(attr, max_jobs, &RealizationPolicy::SubstituteOnly)
        .expect("substitute-only is valid")
}

/// Returns `nix build` arguments for an explicit realization policy.
pub fn cache_shaped_build_args_with_policy(
    attr: &str,
    max_jobs: Option<u32>,
    policy: &RealizationPolicy,
) -> Result<Vec<String>> {
    let mut args = vec![
        "build".to_string(),
        "--no-link".to_string(),
        "--print-out-paths".to_string(),
        attr.to_string(),
    ];
    if let Some(jobs) = max_jobs {
        args.push("--max-jobs".to_string());
        args.push(jobs.to_string());
    }
    args.extend(realization_nix_flags(policy)?);
    Ok(args)
}

/// Returns the realised, non-derivation store paths that must be published for
/// `toplevel`.
///
/// This first asks Nix for the deriver of `toplevel`, then queries the complete
/// closure with outputs included. Empty lines, derivation paths, and paths that
/// are not present on the local filesystem are omitted from the result.
///
/// When the deriver has been garbage-collected (or the path was imported), Nix
/// returns `"unknown-deriver"`. In that case the toplevel path is queried
/// directly for its runtime closure instead.
pub fn closure_to_publish(toplevel: &Path) -> Result<Vec<String>> {
    let deriver = run_nix_store(&["--query", "--deriver"], Some(toplevel))?;
    let deriver = first_non_empty_line(&deriver).ok_or_else(|| Error::NixStoreNoDeriver {
        toplevel: toplevel.display().to_string(),
    })?;

    let query_target = if deriver == "unknown-deriver" {
        toplevel
    } else {
        Path::new(&deriver)
    };

    let requisites = run_nix_store(
        &["--query", "--requisites", "--include-outputs"],
        Some(query_target),
    )?;

    Ok(filter_publish_paths(&requisites))
}

/// Publishes store paths to a shared cache before activation.
pub trait Publisher {
    /// Makes every path present on the shared cache.
    ///
    /// Implementations should be effectively idempotent: paths already present
    /// on the cache should be treated as a successful no-op. Returning `Err`
    /// aborts before activation.
    fn publish(&self, paths: &[String]) -> Result<()>;
}

/// Verifies that store paths are available from a shared cache before activation.
pub trait Verifier {
    /// Confirms every path is queryable on the cache.
    ///
    /// Return the paths that are still missing. Returning `Err`, or returning a
    /// non-empty missing set, aborts before activation.
    fn verify_present(&self, paths: &[String]) -> Result<Vec<String>>;
}

/// A verifier for callers that treat a successful publish as sufficient proof.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrustPublish;

impl Verifier for TrustPublish {
    fn verify_present(&self, _paths: &[String]) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

/// The exact realization passed through build, publication, verification, and
/// activation for one switch attempt.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreparedSwitch {
    /// Frozen flake reference used for the realization.
    pub frozen_flake: String,
    /// Toplevel attribute realized from the frozen flake.
    pub toplevel_attr: String,
    /// Realized system toplevel path.
    pub toplevel: PathBuf,
    /// Complete runtime closure selected for publication.
    pub closure: Vec<String>,
    /// Stable fingerprint of the exact toplevel and closure.
    pub fingerprint: String,
}

/// Inputs for one cache-shaped NixOS rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchPlan<'a> {
    /// Rebuild action, such as `switch`, `boot`, `test`, or `build`.
    pub action: &'a str,
    /// Flake reference passed to `nixos-rebuild --flake`.
    pub flake_attr: &'a str,
    /// Attribute built first to realise the system toplevel.
    pub toplevel_attr: &'a str,
    /// Optional SSH target passed to `nixos-rebuild --target-host`.
    pub target_ssh: Option<&'a str>,
    /// Adds `--use-substitutes` to the activation command.
    pub use_substitutes: bool,
    /// When false, skips publish and verify while keeping cache-shaped flags.
    pub capture: bool,
    /// Lets the caller decide when local activation should run under sudo.
    pub sudo: bool,
    /// Lets the caller decide when remote activation should run under sudo.
    pub remote_sudo: bool,
    /// When set, passed to `nix build` as `--max-jobs` to cap build
    /// parallelism — useful on memory-constrained build hosts where
    /// cross-compilation of many packages in parallel causes OOM.
    pub max_jobs: Option<u32>,
    /// Builder policy used only for the initial toplevel realization.
    pub realization_policy: RealizationPolicy,
}

/// Builds, optionally publishes and verifies, then runs a cache-shaped rebuild.
///
/// Build, publish, and verify failures all return `Err` before activation is
/// attempted. The `build` action stops after capture, matching
/// `nixos-rebuild build` semantics.
pub fn run_cache_shaped_switch(
    plan: &SwitchPlan<'_>,
    publisher: &dyn Publisher,
    verifier: &dyn Verifier,
) -> Result<()> {
    run_cache_shaped_switch_with_runner(plan, publisher, verifier, &ProcessRunner)
}

/// Realizes one switch toplevel and captures its exact closure.
pub fn prepare_switch(plan: &SwitchPlan<'_>) -> Result<PreparedSwitch> {
    prepare_switch_with_runner(plan, &ProcessRunner)
}

trait Runner {
    fn build_toplevel(&self, plan: &SwitchPlan<'_>) -> Result<PathBuf>;
    fn closure_to_publish(&self, toplevel: &Path) -> Result<Vec<String>>;
    fn activate(&self, plan: &SwitchPlan<'_>) -> Result<()>;
}

struct ProcessRunner;

impl Runner for ProcessRunner {
    fn build_toplevel(&self, plan: &SwitchPlan<'_>) -> Result<PathBuf> {
        let attr = plan.toplevel_attr;
        let args =
            cache_shaped_build_args_with_policy(attr, plan.max_jobs, &plan.realization_policy)?;
        eprintln!("crossbow: stage=realize attr={attr}");
        let mut child = Command::new("nix")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| Error::NixBuildRun {
                attr: attr.to_owned(),
                source,
            })?;
        let mut stdout = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(|| Error::NixBuildFailed {
                attr: attr.to_owned(),
                stderr: "nix build stdout was not captured".to_owned(),
            })?
            .read_to_end(&mut stdout)
            .map_err(|source| Error::NixBuildFailed {
                attr: attr.to_owned(),
                stderr: format!("failed to read nix build output: {source}"),
            })?;
        let status = child.wait().map_err(|source| Error::NixBuildRun {
            attr: attr.to_owned(),
            source,
        })?;

        if !status.success() {
            return Err(Error::NixBuildFailed {
                attr: attr.to_owned(),
                stderr: "see streamed nix build stderr".to_owned(),
            });
        }

        let stdout = String::from_utf8(stdout).map_err(|source| Error::NixBuildUtf8 {
            attr: attr.to_owned(),
            source,
        })?;
        let path = first_non_empty_line(&stdout).ok_or_else(|| Error::NixBuildNoOutput {
            attr: attr.to_owned(),
        })?;

        Ok(PathBuf::from(path))
    }

    fn closure_to_publish(&self, toplevel: &Path) -> Result<Vec<String>> {
        closure_to_publish(toplevel)
    }

    fn activate(&self, plan: &SwitchPlan<'_>) -> Result<()> {
        let mut command = if plan.sudo {
            let mut command = Command::new("sudo");
            command.arg("nixos-rebuild");
            command
        } else {
            Command::new("nixos-rebuild")
        };

        command.arg(plan.action).arg("--flake").arg(plan.flake_attr);

        if let Some(target_ssh) = plan.target_ssh {
            command.arg("--target-host").arg(target_ssh);
        }

        if plan.remote_sudo {
            command.arg("--use-remote-sudo");
        }

        command.args(cache_shaped_switch_flags(plan.use_substitutes));

        eprintln!(
            "crossbow: stage=activate action={} flake={}",
            plan.action, plan.flake_attr
        );
        let status = command
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|source| Error::NixosRebuildRun {
                action: plan.action.to_owned(),
                flake_attr: plan.flake_attr.to_owned(),
                source,
            })?;

        if !status.success() {
            return Err(Error::NixosRebuildFailed {
                action: plan.action.to_owned(),
                flake_attr: plan.flake_attr.to_owned(),
                stderr: "see streamed nixos-rebuild stderr".to_owned(),
            });
        }

        Ok(())
    }
}

fn run_cache_shaped_switch_with_runner(
    plan: &SwitchPlan<'_>,
    publisher: &dyn Publisher,
    verifier: &dyn Verifier,
    runner: &dyn Runner,
) -> Result<()> {
    eprintln!(
        "crossbow: stage=prepare action={} flake={}",
        plan.action, plan.flake_attr
    );
    let prepared = prepare_switch_with_runner(plan, runner)?;

    if plan.capture {
        let paths = &prepared.closure;
        eprintln!("crossbow: stage=publish paths={}", paths.len());
        publisher.publish(paths)?;
        eprintln!("crossbow: stage=verify paths={}", paths.len());
        let missing = verifier.verify_present(paths)?;

        if !missing.is_empty() {
            let preview = missing
                .iter()
                .take(5)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::MissingCachePaths {
                count: missing.len(),
                preview,
            });
        }
    }

    if plan.action == "build" {
        return Ok(());
    }

    runner.activate(plan)
}

fn prepare_switch_with_runner(
    plan: &SwitchPlan<'_>,
    runner: &dyn Runner,
) -> Result<PreparedSwitch> {
    let toplevel = runner.build_toplevel(plan)?;
    let closure = if plan.capture {
        runner.closure_to_publish(&toplevel)?
    } else {
        Vec::new()
    };
    let mut hasher = Sha256::new();
    hasher.update(plan.flake_attr.as_bytes());
    hasher.update([0]);
    hasher.update(plan.toplevel_attr.as_bytes());
    hasher.update([0]);
    hasher.update(toplevel.as_os_str().as_encoded_bytes());
    for path in &closure {
        hasher.update([0]);
        hasher.update(path.as_bytes());
    }
    let fingerprint = format!("sha256-{:x}", hasher.finalize());

    Ok(PreparedSwitch {
        frozen_flake: plan.flake_attr.to_owned(),
        toplevel_attr: plan.toplevel_attr.to_owned(),
        toplevel,
        closure,
        fingerprint,
    })
}

fn run_nix_store(args: &[&str], path: Option<&Path>) -> Result<String> {
    let mut command = Command::new("nix-store");
    command.args(args);

    if let Some(path) = path {
        command.arg(path);
    }

    let args_display = args.join(" ");
    let output = command.output().map_err(|source| Error::NixStoreRun {
        args: args_display.clone(),
        source,
    })?;

    if !output.status.success() {
        return Err(Error::NixStoreFailed {
            args: args_display,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    String::from_utf8(output.stdout).map_err(|source| Error::NixStoreUtf8 {
        args: args_display,
        source,
    })
}

fn first_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

fn filter_publish_paths(requisites: &str) -> Vec<String> {
    requisites
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .filter(|path| !path.ends_with(".drv"))
        .filter(|path| Path::new(path).exists())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::rc::Rc;

    struct FakePublisher {
        error: Option<&'static str>,
        seen_paths: Rc<Cell<bool>>,
    }

    impl Publisher for FakePublisher {
        fn publish(&self, paths: &[String]) -> Result<()> {
            self.seen_paths.set(!paths.is_empty());
            if let Some(stderr) = self.error {
                Err(Error::CommandFailed {
                    command: "publish".to_owned(),
                    stderr: stderr.to_owned(),
                })
            } else {
                Ok(())
            }
        }
    }

    struct FakeVerifier {
        missing: Vec<String>,
        error: Option<&'static str>,
    }

    impl Verifier for FakeVerifier {
        fn verify_present(&self, _paths: &[String]) -> Result<Vec<String>> {
            if let Some(stderr) = self.error {
                Err(Error::CommandFailed {
                    command: "verify".to_owned(),
                    stderr: stderr.to_owned(),
                })
            } else {
                Ok(self.missing.clone())
            }
        }
    }

    struct FakeRunner {
        publish_paths: Vec<String>,
        activated: Rc<Cell<bool>>,
    }

    impl Runner for FakeRunner {
        fn build_toplevel(&self, _plan: &SwitchPlan<'_>) -> Result<PathBuf> {
            Ok(PathBuf::from("/nix/store/example-system"))
        }

        fn closure_to_publish(&self, _toplevel: &Path) -> Result<Vec<String>> {
            Ok(self.publish_paths.clone())
        }

        fn activate(&self, _plan: &SwitchPlan<'_>) -> Result<()> {
            self.activated.set(true);
            Ok(())
        }
    }

    fn switch_plan(action: &str) -> SwitchPlan<'_> {
        SwitchPlan {
            action,
            flake_attr: ".#host-crossbow",
            toplevel_attr: ".#nixosConfigurations.host.config.system.build.toplevel",
            target_ssh: Some("root@example"),
            use_substitutes: true,
            capture: true,
            sudo: false,
            remote_sudo: false,
            max_jobs: None,
            realization_policy: RealizationPolicy::SubstituteOnly,
        }
    }

    #[test]
    fn cache_shaped_nix_flags_are_exact() {
        assert_eq!(
            cache_shaped_nix_flags(),
            vec![
                "--builders",
                "",
                "--option",
                "extra-platforms",
                "",
                "--option",
                "always-allow-substitutes",
                "true",
            ]
        );
    }

    #[test]
    fn remote_native_flags_use_only_explicit_builders() {
        let flags = realization_nix_flags(&RealizationPolicy::RemoteNative {
            builders: vec!["ssh-ng://arm aarch64-linux - 1 1".to_string()],
        })
        .unwrap();
        assert_eq!(flags[0], "--builders");
        assert_eq!(flags[1], "ssh-ng://arm aarch64-linux - 1 1");
        assert_eq!(flags[4], "");
    }

    #[test]
    fn remote_native_requires_a_builder() {
        let error = realization_nix_flags(&RealizationPolicy::RemoteNative {
            builders: Vec::new(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("requires at least one"));
    }

    #[test]
    fn cache_shaped_switch_flags_without_substitutes_are_exact() {
        assert_eq!(
            cache_shaped_switch_flags(false),
            vec![
                "--no-reexec",
                "--builders",
                "",
                "--option",
                "extra-platforms",
                "",
                "--option",
                "always-allow-substitutes",
                "true",
            ]
        );
    }

    #[test]
    fn cache_shaped_switch_flags_with_substitutes_are_exact() {
        assert_eq!(
            cache_shaped_switch_flags(true),
            vec![
                "--no-reexec",
                "--builders",
                "",
                "--option",
                "extra-platforms",
                "",
                "--option",
                "always-allow-substitutes",
                "true",
                "--use-substitutes",
            ]
        );
    }

    #[test]
    fn cache_shaped_build_args_disable_builders_and_extra_platforms() {
        assert_eq!(
            cache_shaped_build_args(".#host", None),
            vec![
                "build",
                "--no-link",
                "--print-out-paths",
                ".#host",
                "--builders",
                "",
                "--option",
                "extra-platforms",
                "",
                "--option",
                "always-allow-substitutes",
                "true",
            ]
        );
    }

    #[test]
    fn cache_shaped_build_args_with_max_jobs() {
        assert_eq!(
            cache_shaped_build_args(".#host", Some(4)),
            vec![
                "build",
                "--no-link",
                "--print-out-paths",
                ".#host",
                "--max-jobs",
                "4",
                "--builders",
                "",
                "--option",
                "extra-platforms",
                "",
                "--option",
                "always-allow-substitutes",
                "true",
            ]
        );
    }

    #[test]
    fn filter_publish_paths_drops_blank_derivation_and_missing_paths()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let base =
            std::env::temp_dir().join(format!("crossbow-switch-test-{}", std::process::id()));
        fs::create_dir_all(&base)?;
        let keep = base.join("keep-path");
        fs::write(&keep, "present")?;

        let output = format!(
            "\n{}\n/nix/store/source.drv\n{}\n   \n",
            keep.display(),
            base.join("missing-path").display()
        );

        assert_eq!(
            filter_publish_paths(&output),
            vec![keep.display().to_string()]
        );
        fs::remove_dir_all(base)?;
        Ok(())
    }

    #[test]
    fn prepared_switch_carries_one_exact_closure() {
        let activated = Rc::new(Cell::new(false));
        let runner = FakeRunner {
            publish_paths: vec!["/nix/store/path".to_string()],
            activated,
        };
        let prepared = prepare_switch_with_runner(&switch_plan("build"), &runner).unwrap();

        assert_eq!(prepared.frozen_flake, ".#host-crossbow");
        assert_eq!(
            prepared.toplevel,
            PathBuf::from("/nix/store/example-system")
        );
        assert_eq!(prepared.closure, vec!["/nix/store/path"]);
        assert!(!prepared.fingerprint.is_empty());
    }

    #[test]
    fn trust_publish_reports_no_missing_paths() -> Result<()> {
        assert!(
            TrustPublish
                .verify_present(&["/nix/store/path".to_string()])?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn publish_error_aborts_before_activation() {
        let activated = Rc::new(Cell::new(false));
        let seen_paths = Rc::new(Cell::new(false));
        let runner = FakeRunner {
            publish_paths: vec!["/nix/store/path".to_string()],
            activated: Rc::clone(&activated),
        };
        let publisher = FakePublisher {
            error: Some("publish failed"),
            seen_paths: Rc::clone(&seen_paths),
        };
        let verifier = FakeVerifier {
            missing: Vec::new(),
            error: None,
        };

        let error = run_cache_shaped_switch_with_runner(
            &switch_plan("switch"),
            &publisher,
            &verifier,
            &runner,
        )
        .unwrap_err();

        assert!(error.to_string().contains("publish failed"));
        assert!(seen_paths.get());
        assert!(!activated.get());
    }

    #[test]
    fn verifier_error_aborts_before_activation() {
        let activated = Rc::new(Cell::new(false));
        let runner = FakeRunner {
            publish_paths: vec!["/nix/store/path".to_string()],
            activated: Rc::clone(&activated),
        };
        let publisher = FakePublisher {
            error: None,
            seen_paths: Rc::new(Cell::new(false)),
        };
        let verifier = FakeVerifier {
            missing: Vec::new(),
            error: Some("verify failed"),
        };

        let error = run_cache_shaped_switch_with_runner(
            &switch_plan("switch"),
            &publisher,
            &verifier,
            &runner,
        )
        .unwrap_err();

        assert!(error.to_string().contains("verify failed"));
        assert!(!activated.get());
    }

    #[test]
    fn missing_paths_abort_before_activation() {
        let activated = Rc::new(Cell::new(false));
        let runner = FakeRunner {
            publish_paths: vec!["/nix/store/path".to_string()],
            activated: Rc::clone(&activated),
        };
        let publisher = FakePublisher {
            error: None,
            seen_paths: Rc::new(Cell::new(false)),
        };
        let verifier = FakeVerifier {
            missing: vec!["/nix/store/missing".to_string()],
            error: None,
        };

        let error = run_cache_shaped_switch_with_runner(
            &switch_plan("switch"),
            &publisher,
            &verifier,
            &runner,
        )
        .unwrap_err();

        assert!(error.to_string().contains("1 paths are missing from cache"));
        assert!(!activated.get());
    }

    #[test]
    fn build_action_does_not_activate() -> Result<()> {
        let activated = Rc::new(Cell::new(false));
        let runner = FakeRunner {
            publish_paths: vec!["/nix/store/path".to_string()],
            activated: Rc::clone(&activated),
        };
        let publisher = FakePublisher {
            error: None,
            seen_paths: Rc::new(Cell::new(false)),
        };
        let verifier = FakeVerifier {
            missing: Vec::new(),
            error: None,
        };

        run_cache_shaped_switch_with_runner(&switch_plan("build"), &publisher, &verifier, &runner)?;

        assert!(!activated.get());
        Ok(())
    }
}
