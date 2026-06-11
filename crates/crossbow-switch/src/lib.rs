//! Runtime support for Crossbow cache-shaped NixOS switch orchestration.
//!
//! The crate keeps impure runtime work in Rust: building a toplevel, collecting
//! the closure to publish, checking cache availability, and finally invoking
//! `nixos-rebuild` with QEMU-free cache-shaped flags.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Command-line entry points for the `crossbow-switch` binary.
pub mod cli;
/// Error and result types shared by the library and binary.
pub mod error;
/// Executor descriptor validation for Crossbow check planning.
pub mod executor;
/// Typed access to Crossbow's shared target/profile metadata.
pub mod metadata;

pub use error::{Error, Result};

/// Returns the canonical Nix flags for cache-shaped Crossbow realisation.
///
/// The empty `--builders` and `extra-platforms` values keep activation from
/// falling back to remote builders or binfmt when a host-system path is missing
/// from substituters. `always-allow-substitutes` lets the target substitute
/// paths even when individual derivations set `allowSubstitutes = false`.
#[must_use]
pub fn cache_shaped_nix_flags() -> Vec<String> {
    vec![
        "--builders".to_string(),
        String::new(),
        "--option".to_string(),
        "extra-platforms".to_string(),
        String::new(),
        "--option".to_string(),
        "always-allow-substitutes".to_string(),
        "true".to_string(),
    ]
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
#[must_use]
pub fn cache_shaped_build_args(attr: &str) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        "--no-link".to_string(),
        "--print-out-paths".to_string(),
        attr.to_string(),
    ];
    args.extend(cache_shaped_nix_flags());
    args
}

/// Returns the realised, non-derivation store paths that must be published for
/// `toplevel`.
///
/// This first asks Nix for the deriver of `toplevel`, then queries the complete
/// closure with outputs included. Empty lines, derivation paths, and paths that
/// are not present on the local filesystem are omitted from the result.
pub fn closure_to_publish(toplevel: &Path) -> Result<Vec<String>> {
    let deriver = run_nix_store(&["--query", "--deriver"], Some(toplevel))?;
    let deriver = first_non_empty_line(&deriver).ok_or_else(|| Error::NixStoreNoDeriver {
        toplevel: toplevel.display().to_string(),
    })?;

    let requisites = run_nix_store(
        &["--query", "--requisites", "--include-outputs"],
        Some(Path::new(&deriver)),
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

trait Runner {
    fn build_toplevel(&self, attr: &str) -> Result<PathBuf>;
    fn closure_to_publish(&self, toplevel: &Path) -> Result<Vec<String>>;
    fn activate(&self, plan: &SwitchPlan<'_>) -> Result<()>;
}

struct ProcessRunner;

impl Runner for ProcessRunner {
    fn build_toplevel(&self, attr: &str) -> Result<PathBuf> {
        let args = cache_shaped_build_args(attr);
        let output =
            Command::new("nix")
                .args(&args)
                .output()
                .map_err(|source| Error::NixBuildRun {
                    attr: attr.to_owned(),
                    source,
                })?;

        if !output.status.success() {
            return Err(Error::NixBuildFailed {
                attr: attr.to_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }

        let stdout = String::from_utf8(output.stdout).map_err(|source| Error::NixBuildUtf8 {
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

        command.args(cache_shaped_switch_flags(plan.use_substitutes));

        let output = command.output().map_err(|source| Error::NixosRebuildRun {
            action: plan.action.to_owned(),
            flake_attr: plan.flake_attr.to_owned(),
            source,
        })?;

        if !output.status.success() {
            return Err(Error::NixosRebuildFailed {
                action: plan.action.to_owned(),
                flake_attr: plan.flake_attr.to_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
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
    let toplevel = runner.build_toplevel(plan.toplevel_attr)?;

    if plan.capture {
        let paths = runner.closure_to_publish(&toplevel)?;
        publisher.publish(&paths)?;
        let missing = verifier.verify_present(&paths)?;

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
        fn build_toplevel(&self, _attr: &str) -> Result<PathBuf> {
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
            cache_shaped_build_args(".#host"),
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
