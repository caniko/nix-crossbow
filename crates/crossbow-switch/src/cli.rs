use std::io::Write;
use std::process::{Command, Stdio};

use crate::{
    ClosurePlan, DerivationClass, Error, MissRoute, Publisher, RealizationPolicy, Result,
    RouteHintSpec, SwitchPlan, TrustPublish, Verifier, plan_closure_with_hints,
    run_cache_shaped_switch,
};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed options for one `crossbow-switch` CLI invocation.
pub struct CliOptions {
    /// Rebuild action passed to `nixos-rebuild`.
    pub action: String,
    /// Flake reference passed to `nixos-rebuild --flake`.
    pub flake_attr: String,
    /// Attribute built first to realise the system toplevel.
    pub toplevel_attr: String,
    /// Optional SSH target passed to `nixos-rebuild --target-host`.
    pub target_ssh: Option<String>,
    /// Whether activation should pass `--use-substitutes`.
    pub use_substitutes: bool,
    /// Whether the closure should be published and verified before activation.
    pub capture: bool,
    /// Whether local activation should run `nixos-rebuild` through `sudo`.
    pub sudo: bool,
    /// Whether remote activation should run through sudo on the target host.
    pub remote_sudo: bool,
    /// Shell command that receives store paths on stdin and publishes them.
    pub publish_command: Option<String>,
    /// Shell command that receives store paths on stdin and prints missing paths.
    pub verify_command: Option<String>,
    /// --max-jobs value for nix build, caps build parallelism.
    pub max_jobs: Option<u32>,
    /// Explicit policy for realizing missing host-system derivations.
    pub realization_policy: RealizationPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Parsed options for the read-only structured planner.
pub struct PlanOptions {
    /// Toplevel installable whose complete derivation closure is planned.
    pub toplevel_attr: String,
    /// Crossbow build-host system.
    pub build_system: String,
    /// Target host system.
    pub host_system: String,
    /// Store URLs used for read-only cache probes.
    pub substituters: Vec<String>,
    /// Whether to emit the complete machine-readable report.
    pub json: bool,
    /// Explicit policy for missing derivations.
    pub realization_policy: RealizationPolicy,
    /// Runtime route hints binding installables to miss routes.
    pub route_hints: Vec<RouteHintSpec>,
}

/// Returns the command-line usage string.
#[must_use]
pub fn usage() -> &'static str {
    "usage: crossbow-switch --flake <flake-attr> --toplevel <toplevel-attr> [--action switch|boot|test|build] [--target-host <ssh-host>] [--use-substitutes] [--use-remote-sudo] [--capture --publish-command <command>] [--verify-command <command>] [--sudo] [--max-jobs <N>] [--realization-policy substitute-only|remote-native] [--remote-builder <builder-spec>]"
}

/// Returns the usage string for the read-only planner.
#[must_use]
pub fn plan_usage() -> &'static str {
    "usage: crossbow-switch plan --toplevel <toplevel-attr> --build-system <system> --host-system <system> --substituter <store> [--substituter <store> ...] [--json] [--realization-policy substitute-only|remote-native] [--remote-builder <builder-spec>] [--route-hint <label>@<installable>:<fail|build-local|remote-native>] [--skip-probe <label>]"
}

/// Parses CLI arguments into [`CliOptions`].
///
/// # Errors
///
/// Returns [`Error`] for unknown flags, missing required values,
/// unsupported rebuild actions, or capture mode without a publish command.
pub fn parse_args<I, S>(args: I) -> Result<CliOptions>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut action = "switch".to_owned();
    let mut flake_attr = None;
    let mut toplevel_attr = None;
    let mut target_ssh = None;
    let mut use_substitutes = false;
    let mut capture = false;
    let mut sudo = false;
    let mut remote_sudo = false;
    let mut publish_command = None;
    let mut verify_command = None;
    let mut max_jobs = None;
    let mut realization_policy = "substitute-only".to_owned();
    let mut remote_builders = Vec::new();

    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--action" => {
                index += 1;
                action = required_value(&args, index, "--action")?.to_owned();
            }
            "--flake" => {
                index += 1;
                flake_attr = Some(required_value(&args, index, "--flake")?.to_owned());
            }
            "--toplevel" => {
                index += 1;
                toplevel_attr = Some(required_value(&args, index, "--toplevel")?.to_owned());
            }
            "--target-host" => {
                index += 1;
                target_ssh = Some(required_value(&args, index, "--target-host")?.to_owned());
            }
            "--use-substitutes" => use_substitutes = true,
            "--use-remote-sudo" => remote_sudo = true,
            "--capture" => capture = true,
            "--no-capture" => capture = false,
            "--sudo" => sudo = true,
            "--publish-command" => {
                index += 1;
                publish_command =
                    Some(required_value(&args, index, "--publish-command")?.to_owned());
            }
            "--verify-command" => {
                index += 1;
                verify_command = Some(required_value(&args, index, "--verify-command")?.to_owned());
            }
            "--max-jobs" => {
                index += 1;
                let raw = required_value(&args, index, "--max-jobs")?;
                max_jobs = Some(raw.parse::<u32>().map_err(|_| Error::InvalidMaxJobs {
                    value: raw.to_owned(),
                })?);
            }
            "--realization-policy" => {
                index += 1;
                realization_policy =
                    required_value(&args, index, "--realization-policy")?.to_owned();
            }
            "--remote-builder" => {
                index += 1;
                remote_builders.push(required_value(&args, index, "--remote-builder")?.to_owned());
            }
            "-h" | "--help" => {
                return Err(Error::UnknownArgument {
                    argument: args[index].clone(),
                    usage: usage().to_owned(),
                });
            }
            unknown => {
                return Err(Error::UnknownArgument {
                    argument: unknown.to_owned(),
                    usage: usage().to_owned(),
                });
            }
        }
        index += 1;
    }

    if !matches!(action.as_str(), "switch" | "boot" | "test" | "build") {
        return Err(Error::UnsupportedAction { action });
    }

    if capture && publish_command.is_none() {
        return Err(Error::CaptureRequiresPublishCommand);
    }

    let realization_policy = match realization_policy.as_str() {
        "substitute-only" if remote_builders.is_empty() => RealizationPolicy::SubstituteOnly,
        "remote-native" => RealizationPolicy::RemoteNative {
            builders: remote_builders,
        },
        value => {
            return Err(Error::InvalidRealizationPolicy {
                value: value.to_owned(),
            });
        }
    };

    Ok(CliOptions {
        action,
        flake_attr: flake_attr.ok_or_else(|| Error::MissingRequiredArgument {
            flag: "--flake".to_owned(),
        })?,
        toplevel_attr: toplevel_attr.ok_or_else(|| Error::MissingRequiredArgument {
            flag: "--toplevel".to_owned(),
        })?,
        target_ssh,
        use_substitutes,
        capture,
        sudo,
        remote_sudo,
        publish_command,
        verify_command,
        max_jobs,
        realization_policy,
    })
}

/// Parses arguments for the read-only structured planner.
pub fn parse_plan_args<I, S>(args: I) -> Result<PlanOptions>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let mut toplevel_attr = None;
    let mut build_system = None;
    let mut host_system = None;
    let mut substituters = Vec::new();
    let mut json = false;
    let mut realization_policy = "substitute-only".to_owned();
    let mut remote_builders = Vec::new();
    let mut route_hints = Vec::new();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--toplevel" => {
                index += 1;
                toplevel_attr = Some(required_value(&args, index, "--toplevel")?.to_owned());
            }
            "--build-system" => {
                index += 1;
                build_system = Some(required_value(&args, index, "--build-system")?.to_owned());
            }
            "--host-system" => {
                index += 1;
                host_system = Some(required_value(&args, index, "--host-system")?.to_owned());
            }
            "--substituter" => {
                index += 1;
                substituters.push(required_value(&args, index, "--substituter")?.to_owned());
            }
            "--json" => json = true,
            "--route-hint" => {
                index += 1;
                route_hints.push(parse_route_hint(required_value(
                    &args,
                    index,
                    "--route-hint",
                )?)?);
            }
            "--skip-probe" => {
                index += 1;
                let label = required_value(&args, index, "--skip-probe")?.to_owned();
                let mut found = false;
                for hint in &mut route_hints {
                    if hint.label == label {
                        hint.skip_probe = true;
                        found = true;
                    }
                }
                if !found {
                    return Err(Error::UnknownArgument {
                        argument: format!("--skip-probe {label}"),
                        usage: plan_usage().to_owned(),
                    });
                }
            }
            "--realization-policy" => {
                index += 1;
                realization_policy =
                    required_value(&args, index, "--realization-policy")?.to_owned();
            }
            "--remote-builder" => {
                index += 1;
                remote_builders.push(required_value(&args, index, "--remote-builder")?.to_owned());
            }
            "-h" | "--help" => {
                return Err(Error::UnknownArgument {
                    argument: args[index].clone(),
                    usage: plan_usage().to_owned(),
                });
            }
            unknown => {
                return Err(Error::UnknownArgument {
                    argument: unknown.to_owned(),
                    usage: plan_usage().to_owned(),
                });
            }
        }
        index += 1;
    }

    let realization_policy = match realization_policy.as_str() {
        "substitute-only" if remote_builders.is_empty() => RealizationPolicy::SubstituteOnly,
        "remote-native" => RealizationPolicy::RemoteNative {
            builders: remote_builders,
        },
        value => {
            return Err(Error::InvalidRealizationPolicy {
                value: value.to_owned(),
            });
        }
    };

    Ok(PlanOptions {
        toplevel_attr: toplevel_attr.ok_or_else(|| Error::MissingRequiredArgument {
            flag: "--toplevel".to_owned(),
        })?,
        build_system: build_system.ok_or_else(|| Error::MissingRequiredArgument {
            flag: "--build-system".to_owned(),
        })?,
        host_system: host_system.ok_or_else(|| Error::MissingRequiredArgument {
            flag: "--host-system".to_owned(),
        })?,
        substituters: if substituters.is_empty() {
            return Err(Error::MissingRequiredArgument {
                flag: "--substituter".to_owned(),
            });
        } else {
            substituters
        },
        json,
        realization_policy,
        route_hints,
    })
}

/// Parses process arguments and runs the switch plan.
///
/// # Errors
///
/// Returns [`Error`] when argument parsing fails, when build or
/// activation commands fail, or when publish/verify reports missing cache paths.
pub fn run_from_env() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "plan") {
        let plan_args = &args[1..];
        if plan_args.iter().any(|arg| arg == "-h" || arg == "--help") {
            println!("{}", plan_usage());
            return Ok(());
        }
        let options = parse_plan_args(plan_args.iter().cloned())?;
        let plan = plan_closure_with_hints(
            &options.toplevel_attr,
            &options.build_system,
            &options.host_system,
            &options.substituters,
            &options.realization_policy,
            &options.route_hints,
        )?;
        return print_plan(&plan, options.json);
    }
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", usage());
        return Ok(());
    }

    let options = parse_args(args)?;
    run_with_options(&options)
}

fn print_plan(plan: &ClosurePlan, json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(plan).map_err(|source| Error::PlannerJson { source })?
        );
        return Ok(());
    }

    println!("build-system={}", plan.build_system);
    println!("host-system={}", plan.host_system);
    println!("local-present={}", plan.counts.local_present);
    println!("host-substituted={}", plan.counts.host_substituted);
    println!("build-local={}", plan.counts.build_local);
    println!("host-remote={}", plan.counts.host_remote);
    println!("unhandled={}", plan.counts.unhandled);

    for class in [
        DerivationClass::BuildLocal,
        DerivationClass::HostRemote,
        DerivationClass::Unhandled,
    ] {
        let paths = plan
            .derivations
            .iter()
            .filter(|derivation| derivation.class == class)
            .map(|derivation| derivation.drv.as_str())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            continue;
        }
        println!("{class:?}:");
        for derivation in plan
            .derivations
            .iter()
            .filter(|derivation| derivation.class == class)
        {
            println!("  {} [{}]", derivation.display_label(), derivation.drv);
        }
    }
    Ok(())
}

/// Runs one CLI switch invocation from already-parsed options.
///
/// # Errors
///
/// Returns [`Error`] for the same build, publish, verify, and
/// activation failures as [`crate::run_cache_shaped_switch`].
pub fn run_with_options(options: &CliOptions) -> Result<()> {
    let plan = SwitchPlan {
        action: &options.action,
        flake_attr: &options.flake_attr,
        toplevel_attr: &options.toplevel_attr,
        target_ssh: options.target_ssh.as_deref(),
        use_substitutes: options.use_substitutes,
        capture: options.capture,
        sudo: options.sudo,
        remote_sudo: options.remote_sudo,
        max_jobs: options.max_jobs,
        realization_policy: options.realization_policy.clone(),
    };

    let noop_publisher = NoopPublisher;
    let command_publisher;
    let publisher: &dyn Publisher = if let Some(command) = &options.publish_command {
        command_publisher = CommandPublisher { command };
        &command_publisher
    } else {
        &noop_publisher
    };

    let trust_publish = TrustPublish;
    let command_verifier;
    let verifier: &dyn Verifier = if let Some(command) = &options.verify_command {
        command_verifier = CommandVerifier { command };
        &command_verifier
    } else {
        &trust_publish
    };

    run_cache_shaped_switch(&plan, publisher, verifier)
}

struct NoopPublisher;

impl Publisher for NoopPublisher {
    fn publish(&self, _paths: &[String]) -> Result<()> {
        Err(Error::NoPublisherConfigured)
    }
}

struct CommandPublisher<'a> {
    command: &'a str,
}

impl Publisher for CommandPublisher<'_> {
    fn publish(&self, paths: &[String]) -> Result<()> {
        run_path_command(self.command, paths).map(|_| ())
    }
}

struct CommandVerifier<'a> {
    command: &'a str,
}

impl Verifier for CommandVerifier<'_> {
    fn verify_present(&self, paths: &[String]) -> Result<Vec<String>> {
        let stdout = run_path_command(self.command, paths)?;
        Ok(stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }
}

fn run_path_command(command: &str, paths: &[String]) -> Result<String> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::CommandSpawn {
            command: command.to_owned(),
            source,
        })?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| Error::CommandStdinUnavailable {
                command: command.to_owned(),
            })?;
        for path in paths {
            writeln!(stdin, "{path}").map_err(|source| Error::CommandWriteStdin {
                command: command.to_owned(),
                source,
            })?;
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|source| Error::CommandWait {
            command: command.to_owned(),
            source,
        })?;

    if !output.status.success() {
        return Err(Error::CommandFailed {
            command: command.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    String::from_utf8(output.stdout).map_err(|source| Error::CommandUtf8 {
        command: command.to_owned(),
        source,
    })
}

fn parse_route_hint(spec: &str) -> Result<RouteHintSpec> {
    let (label_installable, route) = spec
        .rsplit_once(':')
        .ok_or_else(|| Error::MissingArgumentValue {
            flag: "--route-hint".to_owned(),
        })?;
    let on_miss = match route {
        "fail" => MissRoute::Fail,
        "build-local" => MissRoute::BuildLocal,
        "remote-native" => MissRoute::RemoteNative,
        value => {
            return Err(Error::UnknownArgument {
                argument: format!("--route-hint route `{value}`"),
                usage: plan_usage().to_owned(),
            });
        }
    };
    let (label, installable) = label_installable
        .split_once('@')
        .ok_or_else(|| Error::MissingArgumentValue {
            flag: "--route-hint".to_owned(),
        })?;
    Ok(RouteHintSpec {
        label: label.to_owned(),
        installable: installable.to_owned(),
        on_miss,
        origin: None,
        skip_probe: false,
    })
}

fn required_value<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str> {
    let value = args.get(index).ok_or_else(|| Error::MissingArgumentValue {
        flag: flag.to_owned(),
    })?;

    if value.starts_with("--") {
        return Err(Error::MissingArgumentValue {
            flag: flag.to_owned(),
        });
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_builds_default_switch_options() -> Result<()> {
        let options = parse_args([
            "--flake",
            ".#host",
            "--toplevel",
            ".#nixosConfigurations.host.config.system.build.toplevel",
        ])?;

        assert_eq!(options.action, "switch");
        assert_eq!(options.flake_attr, ".#host");
        assert!(!options.capture);
        assert!(!options.use_substitutes);
        assert!(!options.remote_sudo);
        assert_eq!(
            options.realization_policy,
            RealizationPolicy::SubstituteOnly
        );
        Ok(())
    }

    #[test]
    fn planner_parser_requires_read_only_inputs() {
        let error = parse_plan_args(["--toplevel", ".#top"]).unwrap_err();
        assert!(error.to_string().contains("--build-system"));
    }

    #[test]
    fn planner_parser_accepts_machine_readable_substitute_only_plan() -> Result<()> {
        let options = parse_plan_args([
            "--toplevel",
            ".#top",
            "--build-system",
            "x86_64-linux",
            "--host-system",
            "aarch64-linux",
            "--substituter",
            "https://cache.example",
            "--json",
        ])?;

        assert!(options.json);
        assert_eq!(options.substituters, vec!["https://cache.example"]);
        assert_eq!(
            options.realization_policy,
            RealizationPolicy::SubstituteOnly
        );
        Ok(())
    }

    #[test]
    fn planner_parser_accepts_repeated_substituters() -> Result<()> {
        let options = parse_plan_args([
            "--toplevel",
            ".#top",
            "--build-system",
            "x86_64-linux",
            "--host-system",
            "aarch64-linux",
            "--substituter",
            "https://private.example",
            "--substituter",
            "https://cache.nixos.org",
        ])?;

        assert_eq!(
            options.substituters,
            vec![
                "https://private.example".to_string(),
                "https://cache.nixos.org".to_string()
            ]
        );
        Ok(())
    }

    #[test]
    fn planner_parser_accepts_route_hints() -> Result<()> {
        let options = parse_plan_args([
            "--toplevel",
            ".#top",
            "--build-system",
            "x86_64-linux",
            "--host-system",
            "aarch64-linux",
            "--substituter",
            "https://cache.example",
            "--route-hint",
            "grpc@.#packages.x86_64-linux.grpc:build-local",
            "--route-hint",
            "plugin@.#plugin:remote-native",
            "--skip-probe",
            "grpc",
        ])?;

        assert_eq!(options.route_hints.len(), 2);
        assert_eq!(options.route_hints[0].label, "grpc");
        assert_eq!(
            options.route_hints[0].installable,
            ".#packages.x86_64-linux.grpc"
        );
        assert_eq!(options.route_hints[0].on_miss, MissRoute::BuildLocal);
        assert!(options.route_hints[0].skip_probe);
        assert_eq!(options.route_hints[1].label, "plugin");
        assert_eq!(options.route_hints[1].on_miss, MissRoute::RemoteNative);
        assert!(!options.route_hints[1].skip_probe);
        Ok(())
    }

    #[test]
    fn planner_parser_rejects_unknown_route_hint_route() {
        let error = parse_plan_args([
            "--toplevel",
            ".#top",
            "--build-system",
            "x86_64-linux",
            "--host-system",
            "aarch64-linux",
            "--substituter",
            "https://cache.example",
            "--route-hint",
            "x@.#x:substitute",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("route `substitute`"));
    }

    #[test]
    fn planner_parser_rejects_unknown_skip_probe_label() {
        let error = parse_plan_args([
            "--toplevel",
            ".#top",
            "--build-system",
            "x86_64-linux",
            "--host-system",
            "aarch64-linux",
            "--substituter",
            "https://cache.example",
            "--skip-probe",
            "nope",
        ])
        .unwrap_err();
        assert!(error.to_string().contains("--skip-probe nope"));
    }

    #[test]
    fn parser_accepts_explicit_remote_native_builder() -> Result<()> {
        let options = parse_args([
            "--flake",
            ".#host",
            "--toplevel",
            ".#top",
            "--realization-policy",
            "remote-native",
            "--remote-builder",
            "ssh-ng://arm aarch64-linux - 1 1",
        ])?;

        assert_eq!(
            options.realization_policy,
            RealizationPolicy::RemoteNative {
                builders: vec!["ssh-ng://arm aarch64-linux - 1 1".to_string()]
            }
        );
        Ok(())
    }

    #[test]
    fn parser_accepts_capture_with_publisher() -> Result<()> {
        let options = parse_args([
            "--action",
            "build",
            "--flake",
            ".#host",
            "--toplevel",
            ".#top",
            "--target-host",
            "root@example",
            "--use-substitutes",
            "--use-remote-sudo",
            "--capture",
            "--publish-command",
            "cat >/tmp/paths",
            "--verify-command",
            "cat >/dev/null",
        ])?;

        assert_eq!(options.action, "build");
        assert_eq!(options.target_ssh.as_deref(), Some("root@example"));
        assert!(options.use_substitutes);
        assert!(options.remote_sudo);
        assert!(options.capture);
        assert_eq!(options.publish_command.as_deref(), Some("cat >/tmp/paths"));
        Ok(())
    }

    #[test]
    fn parser_rejects_capture_without_publisher() {
        let error =
            parse_args(["--flake", ".#host", "--toplevel", ".#top", "--capture"]).unwrap_err();

        assert!(error.to_string().contains("--publish-command"));
    }

    #[test]
    fn parser_rejects_missing_required_flake() {
        let error = parse_args(["--toplevel", ".#top"]).unwrap_err();

        assert!(error.to_string().contains("missing --flake"));
    }

    #[test]
    fn parser_rejects_flag_without_value() {
        let error =
            parse_args(["--flake", ".#host", "--toplevel", ".#top", "--action"]).unwrap_err();

        assert!(error.to_string().contains("--action requires a value"));
    }

    #[test]
    fn parser_no_capture_overrides_capture() -> Result<()> {
        let options = parse_args([
            "--flake",
            ".#host",
            "--toplevel",
            ".#top",
            "--capture",
            "--no-capture",
        ])?;

        assert!(!options.capture);
        Ok(())
    }

    #[test]
    fn parser_rejects_unknown_action() {
        let error = parse_args([
            "--flake",
            ".#host",
            "--toplevel",
            ".#top",
            "--action",
            "dry-run",
        ])
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported nixos-rebuild action")
        );
    }
}
