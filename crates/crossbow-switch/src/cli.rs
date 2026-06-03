use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::{Publisher, SwitchPlan, TrustPublish, Verifier, run_cache_shaped_switch};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    pub action: String,
    pub flake_attr: String,
    pub toplevel_attr: String,
    pub target_ssh: Option<String>,
    pub use_substitutes: bool,
    pub capture: bool,
    pub sudo: bool,
    pub publish_command: Option<String>,
    pub verify_command: Option<String>,
}

pub fn usage() -> &'static str {
    "usage: crossbow-switch --flake <flake-attr> --toplevel <toplevel-attr> [--action switch|boot|test|build] [--target-host <ssh-host>] [--use-substitutes] [--capture --publish-command <command>] [--verify-command <command>] [--sudo]"
}

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
    let mut publish_command = None;
    let mut verify_command = None;

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
            "-h" | "--help" => bail!("{}", usage()),
            unknown => bail!("crossbow: unknown argument `{unknown}`\n{}", usage()),
        }
        index += 1;
    }

    if !matches!(action.as_str(), "switch" | "boot" | "test" | "build") {
        bail!("crossbow: unsupported nixos-rebuild action `{action}`");
    }

    if capture && publish_command.is_none() {
        bail!(
            "crossbow: --capture requires --publish-command; provide the cache publication command or use --no-capture"
        );
    }

    Ok(CliOptions {
        action,
        flake_attr: flake_attr.ok_or_else(|| anyhow!("crossbow: missing --flake"))?,
        toplevel_attr: toplevel_attr.ok_or_else(|| anyhow!("crossbow: missing --toplevel"))?,
        target_ssh,
        use_substitutes,
        capture,
        sudo,
        publish_command,
        verify_command,
    })
}

pub fn run_from_env() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", usage());
        return Ok(());
    }

    let options = parse_args(args)?;
    run_with_options(&options)
}

pub fn run_with_options(options: &CliOptions) -> Result<()> {
    let plan = SwitchPlan {
        action: &options.action,
        flake_attr: &options.flake_attr,
        toplevel_attr: &options.toplevel_attr,
        target_ssh: options.target_ssh.as_deref(),
        use_substitutes: options.use_substitutes,
        capture: options.capture,
        sudo: options.sudo,
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
        bail!("crossbow: no publisher configured")
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
        .with_context(|| format!("spawning command `{command}`"))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow!("command `{command}` did not open stdin"))?;
        for path in paths {
            writeln!(stdin, "{path}")
                .with_context(|| format!("writing store paths to command `{command}`"))?;
        }
    }

    let output = child
        .wait_with_output()
        .with_context(|| format!("waiting for command `{command}`"))?;

    if !output.status.success() {
        bail!(
            "command `{}` failed: {}",
            command,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    String::from_utf8(output.stdout)
        .with_context(|| format!("command `{command}` produced non-UTF-8 output"))
}

fn required_value<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str> {
    let value = args
        .get(index)
        .ok_or_else(|| anyhow!("crossbow: {flag} requires a value"))?;

    if value.starts_with("--") {
        bail!("crossbow: {flag} requires a value");
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
            "--capture",
            "--publish-command",
            "cat >/tmp/paths",
            "--verify-command",
            "cat >/dev/null",
        ])?;

        assert_eq!(options.action, "build");
        assert_eq!(options.target_ssh.as_deref(), Some("root@example"));
        assert!(options.use_substitutes);
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
