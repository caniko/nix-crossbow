use std::io::Write;
use std::process::{Command, Stdio};

use crate::{
    Error, Publisher, Result, SwitchPlan, TrustPublish, Verifier, run_cache_shaped_switch,
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
}

/// Returns the command-line usage string.
#[must_use]
pub fn usage() -> &'static str {
    "usage: crossbow-switch --flake <flake-attr> --toplevel <toplevel-attr> [--action switch|boot|test|build] [--target-host <ssh-host>] [--use-substitutes] [--use-remote-sudo] [--capture --publish-command <command>] [--verify-command <command>] [--sudo] [--max-jobs <N>]"
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
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", usage());
        return Ok(());
    }

    let options = parse_args(args)?;
    run_with_options(&options)
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
