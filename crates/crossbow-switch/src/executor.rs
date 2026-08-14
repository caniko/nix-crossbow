use std::process::Command;

use crate::planner::{ClosurePlan, PLAN_SCHEMA_VERSION, RealizationRoute};
use crate::{Error, Result};

trait CommandRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<()>;
}

struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<()> {
        let command = format!("{program} {}", args.join(" "));
        let output = Command::new(program)
            .args(args)
            .output()
            .map_err(|source| Error::PlannerCommand {
                command: command.clone(),
                source,
            })?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::CommandFailed {
                command,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}

/// Executes an authoritative closure plan without asking Nix to replan it.
///
/// Actions run in the dependency-first order recorded in the plan. Every
/// expected output is validated through the local Nix daemon after its action.
pub fn execute_plan(plan: &ClosurePlan, max_jobs: Option<u32>) -> Result<()> {
    execute_plan_with_runner(plan, max_jobs, &ProcessRunner)
}

fn execute_plan_with_runner(
    plan: &ClosurePlan,
    max_jobs: Option<u32>,
    runner: &dyn CommandRunner,
) -> Result<()> {
    if plan.schema_version != PLAN_SCHEMA_VERSION {
        return Err(Error::UnsupportedPlanSchema {
            version: plan.schema_version,
        });
    }
    let actual = plan.canonical_plan_id()?;
    if plan.plan_id != actual {
        return Err(Error::PlanIdMismatch {
            expected: plan.plan_id.clone(),
            actual,
        });
    }
    if let Some((path, reason)) = plan.derivations.iter().find_map(|derivation| {
        derivation
            .actions
            .iter()
            .find_map(|action| match &action.route {
                RealizationRoute::Fail { reason } => Some((action.path.clone(), reason.clone())),
                _ => None,
            })
    }) {
        return Err(Error::PlanBlocked { path, reason });
    }

    for derivation in &plan.derivations {
        for action in &derivation.actions {
            let args = match &action.route {
                RealizationRoute::Substitute { substituter } => vec![
                    "copy".to_owned(),
                    "--from".to_owned(),
                    substituter.clone(),
                    action.path.clone(),
                ],
                RealizationRoute::AlreadyPresent => Vec::new(),
                RealizationRoute::BuildLocal => {
                    realization_args(&derivation.drv, &action.output, &[], max_jobs, false)
                }
                RealizationRoute::RemoteNative { builders } => {
                    realization_args(&derivation.drv, &action.output, builders, max_jobs, true)
                }
                RealizationRoute::Fail { .. } => unreachable!("blocked plans return above"),
            };
            if !args.is_empty() {
                runner.run("nix", &args)?;
            }
            runner.run(
                "nix",
                &[
                    "path-info".to_owned(),
                    "--store".to_owned(),
                    "daemon".to_owned(),
                    action.path.clone(),
                ],
            )?;
        }
    }
    Ok(())
}

fn realization_args(
    drv: &str,
    output: &str,
    builders: &[String],
    max_jobs: Option<u32>,
    remote_only: bool,
) -> Vec<String> {
    let mut args = vec![
        "build".to_owned(),
        "--no-link".to_owned(),
        format!("{drv}^{output}"),
        "--builders".to_owned(),
        builders.join("\n"),
        "--option".to_owned(),
        "substitute".to_owned(),
        "false".to_owned(),
        "--option".to_owned(),
        "fallback".to_owned(),
        "false".to_owned(),
        "--option".to_owned(),
        "extra-platforms".to_owned(),
        String::new(),
    ];
    if remote_only {
        args.extend(["--max-jobs".to_owned(), "0".to_owned()]);
    } else if let Some(jobs) = max_jobs {
        args.extend(["--max-jobs".to_owned(), jobs.to_string()]);
    }
    args
}

/// Executor descriptor accepted by Crossbow check planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorDescriptor {
    /// Check by requiring a native builder for the host system.
    NativeBuilder {
        /// Optional builder names accepted by the descriptor.
        builders: Vec<String>,
    },
    /// Mark the check as intentionally skipped with a human-readable reason.
    Skip {
        /// Human-readable skip reason.
        reason: String,
    },
    /// Declared WASI executor stub.
    Wasmtime {
        /// Whether a concrete wasmtime package was supplied by the caller.
        configured: bool,
    },
    /// Declared Windows/Wine executor stub.
    Wine {
        /// Whether a concrete wine package was supplied by the caller.
        configured: bool,
    },
}

/// Planned check behavior derived from an [`ExecutorDescriptor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckPlan {
    /// Native-builder checks record the system that must be executable.
    NativeBuilder {
        /// Host system required by the check.
        required_system: String,
    },
    /// Skipped checks emit the supplied reason.
    Skipped {
        /// Human-readable skip reason.
        reason: String,
    },
}

/// Validates an executor descriptor for a package check.
///
/// # Errors
///
/// Returns [`Error::NativeBuilderMissingHost`] when a native-builder check has
/// no host system, and [`Error::ExecutorNotImplemented`] for declared stubs.
pub fn plan_executor_check(
    executor: &ExecutorDescriptor,
    host_system: &str,
    check_name: &str,
) -> Result<CheckPlan> {
    match executor {
        ExecutorDescriptor::Skip { reason } => Ok(CheckPlan::Skipped {
            reason: reason.clone(),
        }),
        ExecutorDescriptor::NativeBuilder { .. } => {
            if host_system.is_empty() {
                return Err(Error::NativeBuilderMissingHost {
                    check_name: check_name.to_owned(),
                });
            }

            Ok(CheckPlan::NativeBuilder {
                required_system: host_system.to_owned(),
            })
        }
        ExecutorDescriptor::Wasmtime { .. } => Err(Error::ExecutorNotImplemented {
            kind: "wasmtime".to_owned(),
        }),
        ExecutorDescriptor::Wine { .. } => Err(Error::ExecutorNotImplemented {
            kind: "wine".to_owned(),
        }),
    }
}

/// Parses an executor kind name into an [`ExecutorDescriptor`].
///
/// # Errors
///
/// Returns [`Error::UnknownExecutor`] for names Crossbow does not recognize.
pub fn parse_executor_kind(kind: &str) -> Result<ExecutorDescriptor> {
    match kind {
        "native-builder" => Ok(ExecutorDescriptor::NativeBuilder {
            builders: Vec::new(),
        }),
        "skip" => Ok(ExecutorDescriptor::Skip {
            reason: "crossbow check skipped".to_owned(),
        }),
        "wasmtime" => Ok(ExecutorDescriptor::Wasmtime { configured: false }),
        "wine" => Ok(ExecutorDescriptor::Wine { configured: false }),
        other => Err(Error::UnknownExecutor {
            kind: other.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{
        DerivationClass, DerivationPlan, OutputAction, PlanCounts, PlanDependency, PlanRoot,
        ProbeResult,
    };
    use std::cell::RefCell;

    #[test]
    fn native_builder_requires_host_system() {
        let error = plan_executor_check(
            &ExecutorDescriptor::NativeBuilder {
                builders: Vec::new(),
            },
            "",
            "tiny",
        )
        .unwrap_err();

        assert!(error.to_string().contains("non-empty host system"));
    }

    #[test]
    fn native_builder_returns_required_system() -> Result<()> {
        let plan = plan_executor_check(
            &ExecutorDescriptor::NativeBuilder {
                builders: Vec::new(),
            },
            "aarch64-linux",
            "tiny",
        )?;

        assert_eq!(
            plan,
            CheckPlan::NativeBuilder {
                required_system: "aarch64-linux".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn skip_executor_preserves_reason() -> Result<()> {
        let plan = plan_executor_check(
            &ExecutorDescriptor::Skip {
                reason: "no runner".to_owned(),
            },
            "wasm32-wasi",
            "tiny",
        )?;

        assert_eq!(
            plan,
            CheckPlan::Skipped {
                reason: "no runner".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn parse_executor_kind_accepts_known_kinds() -> Result<()> {
        assert_eq!(
            parse_executor_kind("native-builder")?,
            ExecutorDescriptor::NativeBuilder {
                builders: Vec::new(),
            }
        );
        assert_eq!(
            parse_executor_kind("skip")?,
            ExecutorDescriptor::Skip {
                reason: "crossbow check skipped".to_owned(),
            }
        );
        assert_eq!(
            parse_executor_kind("wasmtime")?,
            ExecutorDescriptor::Wasmtime { configured: false }
        );
        assert_eq!(
            parse_executor_kind("wine")?,
            ExecutorDescriptor::Wine { configured: false }
        );
        Ok(())
    }

    #[test]
    fn parse_executor_kind_rejects_unknown_kind() {
        let error = parse_executor_kind("qemu").unwrap_err();

        assert!(error.to_string().contains("unknown executor `qemu`"));
    }

    #[test]
    fn declared_stub_executors_report_phase_1_error() {
        let wasmtime = plan_executor_check(
            &ExecutorDescriptor::Wasmtime { configured: false },
            "x",
            "tiny",
        )
        .unwrap_err();
        let wine =
            plan_executor_check(&ExecutorDescriptor::Wine { configured: false }, "x", "tiny")
                .unwrap_err();

        assert!(wasmtime.to_string().contains("not implemented in phase 1"));
        assert!(wine.to_string().contains("not implemented in phase 1"));
    }

    #[derive(Default)]
    struct RecordingRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<()> {
            self.calls
                .borrow_mut()
                .push((program.to_owned(), args.to_vec()));
            Ok(())
        }
    }

    fn action(output: &str, path: &str, route: RealizationRoute) -> OutputAction {
        OutputAction {
            output: output.to_owned(),
            path: path.to_owned(),
            probe: match &route {
                RealizationRoute::Substitute { substituter } => ProbeResult::Present {
                    substituter: substituter.clone(),
                },
                RealizationRoute::AlreadyPresent => ProbeResult::LocalPresent,
                _ => ProbeResult::Missing,
            },
            route,
        }
    }

    fn executable_plan() -> ClosurePlan {
        let dependency = DerivationPlan {
            drv: "/nix/store/dependency.drv".to_owned(),
            name: None,
            pname: None,
            output_names: vec!["out".to_owned()],
            outputs: vec!["/nix/store/dependency".to_owned()],
            system: "x86_64-linux".to_owned(),
            class: DerivationClass::BuildLocal,
            route: RealizationRoute::BuildLocal,
            hint: None,
            probe: Some(ProbeResult::Missing),
            actions: vec![action(
                "out",
                "/nix/store/dependency",
                RealizationRoute::BuildLocal,
            )],
            dependencies: vec![],
        };
        let root = DerivationPlan {
            drv: "/nix/store/root.drv".to_owned(),
            name: None,
            pname: None,
            output_names: vec!["dev".to_owned(), "out".to_owned()],
            outputs: vec![
                "/nix/store/root-dev".to_owned(),
                "/nix/store/root".to_owned(),
            ],
            system: "x86_64-linux".to_owned(),
            class: DerivationClass::BuildLocal,
            route: RealizationRoute::BuildLocal,
            hint: None,
            probe: Some(ProbeResult::Missing),
            actions: vec![
                action(
                    "dev",
                    "/nix/store/root-dev",
                    RealizationRoute::Substitute {
                        substituter: "https://first.example".to_owned(),
                    },
                ),
                action("out", "/nix/store/root", RealizationRoute::BuildLocal),
            ],
            dependencies: vec![PlanDependency {
                drv: dependency.drv.clone(),
                outputs: vec!["out".to_owned()],
            }],
        };
        let mut plan = ClosurePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            toplevel_installable: "/nix/store/source#top".to_owned(),
            flake_installable: "/nix/store/source#host".to_owned(),
            build_system: "x86_64-linux".to_owned(),
            host_system: "aarch64-linux".to_owned(),
            roots: vec![PlanRoot {
                drv: root.drv.clone(),
                outputs: vec!["out".to_owned()],
            }],
            derivations: vec![dependency, root],
            counts: PlanCounts {
                build_local: 2,
                ..PlanCounts::default()
            },
        };
        plan.plan_id = plan.canonical_plan_id().unwrap();
        plan
    }

    #[test]
    fn executes_recorded_mixed_routes_in_dependency_order() {
        let runner = RecordingRunner::default();
        execute_plan_with_runner(&executable_plan(), Some(2), &runner).unwrap();
        let calls = runner.calls.borrow();

        assert_eq!(calls[0].1[2], "/nix/store/dependency.drv^out");
        assert!(
            calls[0]
                .1
                .windows(3)
                .any(|args| args == ["--option", "substitute", "false"])
        );
        assert!(
            calls[0]
                .1
                .windows(3)
                .any(|args| args == ["--option", "fallback", "false"])
        );
        assert!(calls[0].1.windows(2).any(|args| args == ["--builders", ""]));
        assert_eq!(
            calls[2].1,
            [
                "copy",
                "--from",
                "https://first.example",
                "/nix/store/root-dev"
            ]
        );
        assert_eq!(calls[4].1[2], "/nix/store/root.drv^out");
        assert_eq!(
            calls[5].1,
            ["path-info", "--store", "daemon", "/nix/store/root"]
        );
    }

    #[test]
    fn rejects_modified_plan_before_running_commands() {
        let runner = RecordingRunner::default();
        let mut plan = executable_plan();
        plan.derivations[0].actions[0].path = "/nix/store/tampered".to_owned();

        assert!(matches!(
            execute_plan_with_runner(&plan, None, &runner),
            Err(Error::PlanIdMismatch { .. })
        ));
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn executes_builtin_dependency_before_consumer() {
        let runner = RecordingRunner::default();
        let mut plan = executable_plan();
        plan.derivations[0].system = "builtin".to_owned();
        plan.plan_id = plan.canonical_plan_id().unwrap();

        execute_plan_with_runner(&plan, None, &runner).unwrap();
        let calls = runner.calls.borrow();
        assert_eq!(calls[0].1[2], "/nix/store/dependency.drv^out");
        assert_eq!(calls[2].1[0], "copy");
    }

    #[test]
    fn realization_flags_separate_local_and_recorded_remote_routes() {
        let local = realization_args("/nix/store/a.drv", "out", &[], Some(3), false);
        assert!(
            local
                .windows(3)
                .any(|args| args == ["--option", "substitute", "false"])
        );
        assert!(local.windows(2).any(|args| args == ["--builders", ""]));
        assert!(local.windows(2).any(|args| args == ["--max-jobs", "3"]));

        let remote = realization_args(
            "/nix/store/a.drv",
            "out",
            &["ssh-ng://arm aarch64-linux - 1 1".to_owned()],
            Some(3),
            true,
        );
        assert!(
            remote
                .windows(2)
                .any(|args| { args == ["--builders", "ssh-ng://arm aarch64-linux - 1 1"] })
        );
        assert!(remote.windows(2).any(|args| args == ["--max-jobs", "0"]));
        assert!(!remote.windows(2).any(|args| args == ["--max-jobs", "3"]));
    }
}
