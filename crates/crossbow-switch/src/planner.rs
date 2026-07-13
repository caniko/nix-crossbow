//! Structured planning for cache-shaped Crossbow realizations.

use std::collections::BTreeMap;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{Error, RealizationPolicy, Result};

/// The realization class assigned to one derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivationClass {
    /// The output is available from the queried substituter.
    HostSubstituted,
    /// The derivation will be built on the Crossbow build host.
    BuildLocal,
    /// The host-system miss is eligible for a declared native builder.
    HostRemote,
    /// The derivation does not match either declared platform.
    Unhandled,
}

/// One derivation discovered by `nix build --dry-run --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivationPlan {
    /// Full derivation store path.
    pub drv: String,
    /// Output store paths reported by Nix.
    pub outputs: Vec<String>,
    /// Nix's build system for the derivation.
    pub system: String,
    /// Classification under the selected realization policy.
    pub class: DerivationClass,
}

/// Counts for a structured closure plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanCounts {
    /// Outputs already available from substituters.
    pub host_substituted: usize,
    /// Derivations Nix will build on the build host.
    pub build_local: usize,
    /// Host-system misses routed to a declared native builder.
    pub host_remote: usize,
    /// Derivations that could not be safely classified.
    pub unhandled: usize,
}

/// Complete machine-readable realization plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosurePlan {
    /// Build system running Crossbow.
    pub build_system: String,
    /// Target host system.
    pub host_system: String,
    /// Derivations in the dry-run frontier.
    pub derivations: Vec<DerivationPlan>,
    /// Aggregate class counts.
    pub counts: PlanCounts,
}

#[derive(Debug, Deserialize)]
struct DryRunEntry {
    #[serde(rename = "drvPath")]
    drv_path: String,
    outputs: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct DryRunDerivations {
    derivations: BTreeMap<String, DryRunDerivation>,
}

#[derive(Debug, Deserialize)]
struct DryRunDerivation {
    system: String,
}

/// Computes a structured plan from Nix's machine-readable dry-run output.
///
/// Nix's dry-run JSON gives the derivation frontier and outputs. A second
/// `nix derivation show` call supplies each derivation's build system. Cache
/// availability is queried through the supplied substituter store URL. Human
/// stderr is never parsed.
pub fn plan_closure(
    attr: &str,
    build_system: &str,
    host_system: &str,
    substituter: &str,
    policy: &RealizationPolicy,
) -> Result<ClosurePlan> {
    let mut dry_run_args = ["build", "--dry-run", "--json", "--no-link", attr]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    dry_run_args.extend(crate::realization_nix_flags(policy)?);
    let dry_run = run_nix(&dry_run_args)?;
    let entries: Vec<DryRunEntry> =
        serde_json::from_str(&dry_run).map_err(|source| Error::PlannerJson { source })?;
    if entries.is_empty() {
        return Ok(ClosurePlan {
            build_system: build_system.to_owned(),
            host_system: host_system.to_owned(),
            derivations: Vec::new(),
            counts: PlanCounts::default(),
        });
    }

    let drv_paths = entries
        .iter()
        .map(|entry| entry.drv_path.as_str())
        .collect::<Vec<_>>();
    let mut show_args = vec!["derivation".to_string(), "show".to_string()];
    show_args.extend(drv_paths.iter().map(|path| (*path).to_owned()));
    let shown = run_nix(&show_args)?;
    let shown: DryRunDerivations =
        serde_json::from_str(&shown).map_err(|source| Error::PlannerJson { source })?;

    let mut derivations = Vec::with_capacity(entries.len());
    let mut counts = PlanCounts::default();
    for entry in entries {
        let key = entry
            .drv_path
            .strip_prefix("/nix/store/")
            .unwrap_or(&entry.drv_path);
        let system = shown
            .derivations
            .get(key)
            .map(|value| value.system.clone())
            .ok_or_else(|| Error::PlannerMissingDerivation {
                drv: entry.drv_path.clone(),
            })?;
        let outputs = entry.outputs.into_values().collect::<Vec<_>>();
        let substitutable = outputs
            .iter()
            .all(|output| output_available(substituter, output));
        let class = if substitutable {
            DerivationClass::HostSubstituted
        } else if system == build_system {
            DerivationClass::BuildLocal
        } else if system == host_system {
            match policy {
                RealizationPolicy::RemoteNative { builders } if !builders.is_empty() => {
                    DerivationClass::HostRemote
                }
                RealizationPolicy::SubstituteOnly
                | RealizationPolicy::RemoteNative { builders: _ } => DerivationClass::Unhandled,
            }
        } else {
            DerivationClass::Unhandled
        };

        match class {
            DerivationClass::HostSubstituted => counts.host_substituted += 1,
            DerivationClass::BuildLocal => counts.build_local += 1,
            DerivationClass::HostRemote => counts.host_remote += 1,
            DerivationClass::Unhandled => counts.unhandled += 1,
        }
        derivations.push(DerivationPlan {
            drv: entry.drv_path,
            outputs,
            system,
            class,
        });
    }

    Ok(ClosurePlan {
        build_system: build_system.to_owned(),
        host_system: host_system.to_owned(),
        derivations,
        counts,
    })
}

fn run_nix(args: &[String]) -> Result<String> {
    let command = format!("nix {}", args.join(" "));
    let output =
        Command::new("nix")
            .args(args)
            .output()
            .map_err(|source| Error::PlannerCommand {
                command: command.clone(),
                source,
            })?;
    if !output.status.success() {
        return Err(Error::PlannerCommandFailed {
            command,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|source| Error::PlannerUtf8 { command, source })
}

fn output_available(substituter: &str, output: &str) -> bool {
    Command::new("nix")
        .args(["path-info", "--json", "--store", substituter, output])
        .output()
        .is_ok_and(|result| result.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_machine_readable_plan() {
        let plan = ClosurePlan {
            build_system: "x86_64-linux".into(),
            host_system: "aarch64-linux".into(),
            derivations: vec![],
            counts: PlanCounts::default(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        assert!(json.contains("build_system"));
        assert!(json.contains("host_substituted"));
    }
}
