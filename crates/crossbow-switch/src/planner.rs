//! Structured planning for cache-shaped Crossbow realizations.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// One derivation discovered from Nix's derivation closure.
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
    /// Outputs already available from the configured substituters.
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
    /// Derivations in the realized closure graph.
    pub derivations: Vec<DerivationPlan>,
    /// Aggregate class counts.
    pub counts: PlanCounts,
}

#[derive(Debug, Deserialize)]
struct DerivationGraph {
    derivations: BTreeMap<String, DerivationNode>,
}

#[derive(Debug, Deserialize)]
struct DerivationNode {
    system: String,
    outputs: BTreeMap<String, DerivationOutput>,
    #[serde(default)]
    inputs: DerivationInputs,
}

#[derive(Debug, Default, Deserialize)]
struct DerivationInputs {
    #[serde(default)]
    drvs: BTreeMap<String, DerivationInput>,
}

#[derive(Debug, Default, Deserialize)]
struct DerivationInput {
    #[serde(default)]
    outputs: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct DerivationOutput {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OutputEntry {
    #[serde(rename = "drvPath")]
    drv_path: String,
    outputs: BTreeMap<String, String>,
}

/// Computes a structured plan from Nix's derivation closure.
///
/// `nix build --dry-run --json` reports only the requested installable, not its
/// complete action set. Nix's local derivation closure is enumerated first and
/// then shown in bounded batches; output paths and substituter availability are
/// resolved in batches. Human stderr is never parsed.
pub fn plan_closure(
    attr: &str,
    build_system: &str,
    host_system: &str,
    substituter: &str,
    policy: &RealizationPolicy,
) -> Result<ClosurePlan> {
    plan_closure_with_substituters(
        attr,
        build_system,
        host_system,
        &[substituter.to_owned()],
        policy,
    )
}

/// Computes a structured plan using the union of the supplied substituters.
///
/// The order is significant only for efficiency: each later substituter is
/// queried for outputs still missing from the earlier ones. Duplicate URLs
/// are ignored. At least one substituter is required so a failed configuration
/// cannot be mistaken for a cache-shaped closure.
pub fn plan_closure_with_substituters(
    attr: &str,
    build_system: &str,
    host_system: &str,
    substituters: &[String],
    policy: &RealizationPolicy,
) -> Result<ClosurePlan> {
    let root_args = ["derivation", "show", "--no-pretty", attr]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let roots = parse_graph(&run_nix(&root_args)?)?;
    if roots.derivations.is_empty() {
        return Ok(ClosurePlan {
            build_system: build_system.to_owned(),
            host_system: host_system.to_owned(),
            derivations: Vec::new(),
            counts: PlanCounts::default(),
        });
    }

    let graph = load_derivation_graph(&roots)?;
    let required = required_outputs(&roots, &graph);
    let output_paths = resolve_output_paths(&graph, &required, policy)?;
    let all_outputs = output_paths
        .values()
        .flat_map(|paths| paths.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let availability = probe_outputs(substituters, &all_outputs)?;

    let mut derivations = Vec::with_capacity(graph.derivations.len());
    let mut counts = PlanCounts::default();
    for (key, node) in &graph.derivations {
        if node.system == "builtin" {
            continue;
        }
        let Some(outputs) = output_paths.get(key) else {
            continue;
        };
        let substitutable = outputs
            .iter()
            .all(|output| availability.get(output).copied().unwrap_or(false));
        let system = node.system.clone();
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
            drv: store_path(key),
            outputs: outputs.clone(),
            system,
            class,
        });
    }

    derivations.sort_by(|left, right| left.drv.cmp(&right.drv));

    Ok(ClosurePlan {
        build_system: build_system.to_owned(),
        host_system: host_system.to_owned(),
        derivations,
        counts,
    })
}

fn parse_graph(value: &str) -> Result<DerivationGraph> {
    serde_json::from_str(value).map_err(|source| Error::PlannerJson { source })
}

fn load_derivation_graph(roots: &DerivationGraph) -> Result<DerivationGraph> {
    let mut closure_args = vec!["-q".to_owned(), "--requisites".to_owned()];
    closure_args.extend(roots.derivations.keys().map(|key| store_path(key)));
    let closure = run_command(
        "nix-store",
        &closure_args,
        format!(
            "nix-store -q --requisites root-count={}",
            roots.derivations.len()
        ),
    )?;
    let drv_paths = parse_drv_paths(&closure);
    if drv_paths.is_empty() {
        return Err(Error::PlannerMissingDerivation {
            drv: roots
                .derivations
                .keys()
                .next()
                .map(|key| store_path(key))
                .unwrap_or_default(),
        });
    }

    let mut derivations = BTreeMap::new();
    for chunk in drv_paths.iter().collect::<Vec<_>>().chunks(256) {
        let mut args = vec![
            "derivation".to_owned(),
            "show".to_owned(),
            "--no-pretty".to_owned(),
        ];
        args.extend(chunk.iter().map(|path| (*path).clone()));
        let batch = parse_graph(&run_command(
            "nix",
            &args,
            format!("nix derivation show batch-count={}", chunk.len()),
        )?)?;
        derivations.extend(batch.derivations);
    }
    Ok(DerivationGraph { derivations })
}

fn parse_drv_paths(value: &str) -> BTreeSet<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|path| path.ends_with(".drv"))
        .map(store_path)
        .collect()
}

fn derivation_key(path: &str) -> String {
    path.strip_prefix("/nix/store/").unwrap_or(path).to_owned()
}

fn store_path(path: &str) -> String {
    if path.starts_with("/nix/store/") {
        path.to_owned()
    } else {
        format!("/nix/store/{path}")
    }
}

fn required_outputs(
    roots: &DerivationGraph,
    graph: &DerivationGraph,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut required = BTreeMap::<String, BTreeSet<String>>::new();
    for key in roots.derivations.keys() {
        if let Some(node) = graph.derivations.get(key) {
            required
                .entry(derivation_key(key))
                .or_default()
                .extend(node.outputs.keys().cloned());
        }
    }
    for node in graph.derivations.values() {
        for (key, input) in &node.inputs.drvs {
            required
                .entry(derivation_key(key))
                .or_default()
                .extend(input.outputs.iter().cloned());
        }
    }
    required
}

fn resolve_output_paths(
    graph: &DerivationGraph,
    required: &BTreeMap<String, BTreeSet<String>>,
    policy: &RealizationPolicy,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut resolved = BTreeMap::<(String, String), String>::new();
    let mut pending = Vec::<(String, String, String)>::new();

    for (key, names) in required {
        let Some(node) = graph.derivations.get(key) else {
            return Err(Error::PlannerMissingDerivation {
                drv: store_path(key),
            });
        };
        if node.system == "builtin" {
            continue;
        }
        for name in names {
            let output = node
                .outputs
                .get(name)
                .ok_or_else(|| Error::PlannerMissingOutput {
                    drv: store_path(key),
                    output: name.clone(),
                })?;
            match &output.path {
                Some(path) => {
                    resolved.insert((key.clone(), name.clone()), store_path(path));
                }
                None => pending.push((
                    key.clone(),
                    name.clone(),
                    format!("{}^{}", store_path(key), name),
                )),
            }
        }
    }

    for chunk in pending.chunks(256) {
        let mut args = ["build", "--dry-run", "--json", "--no-link"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        args.extend(chunk.iter().map(|(_, _, spec)| spec.clone()));
        args.extend(crate::realization_nix_flags(policy)?);
        let output = run_command_silent(
            "nix",
            &args,
            format!("nix build --dry-run --json output-count={}", chunk.len()),
        )?;
        let entries: Vec<OutputEntry> =
            serde_json::from_str(&output).map_err(|source| Error::PlannerJson { source })?;
        for entry in entries {
            let key = derivation_key(&entry.drv_path);
            for (name, path) in entry.outputs {
                resolved.insert((key.clone(), name), store_path(&path));
            }
        }
    }

    let mut paths = BTreeMap::new();
    for (key, names) in required {
        if graph
            .derivations
            .get(key)
            .is_some_and(|node| node.system == "builtin")
        {
            continue;
        }
        let mut output_paths = Vec::with_capacity(names.len());
        for name in names {
            let path = resolved
                .get(&(key.clone(), name.clone()))
                .cloned()
                .ok_or_else(|| Error::PlannerMissingOutput {
                    drv: store_path(key),
                    output: name.clone(),
                })?;
            output_paths.push(path);
        }
        paths.insert(key.clone(), output_paths);
    }
    Ok(paths)
}

fn run_nix(args: &[String]) -> Result<String> {
    run_command("nix", args, format!("nix {}", args.join(" ")))
}

fn run_command(program: &str, args: &[String], command: String) -> Result<String> {
    run_command_with_stderr(program, args, command, Stdio::inherit())
}

fn run_command_silent(program: &str, args: &[String], command: String) -> Result<String> {
    run_command_with_stderr(program, args, command, Stdio::null())
}

fn run_command_with_stderr(
    program: &str,
    args: &[String],
    command: String,
    stderr: Stdio,
) -> Result<String> {
    eprintln!("crossbow: stage=plan command={command}");
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .map_err(|source| Error::PlannerCommand {
            command: command.clone(),
            source,
        })?;
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| Error::PlannerCommandFailed {
            command: command.clone(),
            stderr: "planner stdout was not captured".to_owned(),
        })?
        .read_to_end(&mut stdout)
        .map_err(|source| Error::PlannerCommandFailed {
            command: command.clone(),
            stderr: format!("failed to read planner output: {source}"),
        })?;
    let status = child.wait().map_err(|source| Error::PlannerCommand {
        command: command.clone(),
        source,
    })?;
    if !status.success() {
        return Err(Error::PlannerCommandFailed {
            command,
            stderr: "planner command failed".to_owned(),
        });
    }
    String::from_utf8(stdout).map_err(|source| Error::PlannerUtf8 { command, source })
}

fn probe_outputs(substituters: &[String], outputs: &[String]) -> Result<BTreeMap<String, bool>> {
    if substituters.is_empty() {
        return Err(Error::PlannerNoSubstituters);
    }

    let mut availability = outputs
        .iter()
        .cloned()
        .map(|output| (output, false))
        .collect::<BTreeMap<_, _>>();
    let mut pending = outputs.to_vec();
    let mut queried = BTreeSet::new();

    for substituter in substituters {
        if !queried.insert(substituter) || pending.is_empty() {
            continue;
        }

        let probed = probe_substituter_outputs(substituter, &pending)?;
        merge_availability(&mut availability, probed);
        pending.retain(|output| !availability.get(output).copied().unwrap_or(false));
    }

    Ok(availability)
}

fn merge_availability(availability: &mut BTreeMap<String, bool>, probed: BTreeMap<String, bool>) {
    for (output, present) in probed {
        if present {
            availability.insert(output, true);
        }
    }
}

fn probe_substituter_outputs(
    substituter: &str,
    outputs: &[String],
) -> Result<BTreeMap<String, bool>> {
    let mut availability = BTreeMap::new();
    for chunk in outputs.chunks(1024) {
        let mut args = vec![
            "path-info".to_owned(),
            "--json".to_owned(),
            "--json-format".to_owned(),
            "1".to_owned(),
            "--store".to_owned(),
            substituter.to_owned(),
        ];
        args.extend(chunk.iter().cloned());
        let result = Command::new("nix").args(&args).output().map_err(|source| {
            Error::PlannerCacheProbe {
                substituter: substituter.to_owned(),
                output: chunk.first().cloned().unwrap_or_default(),
                source,
            }
        })?;
        let values = serde_json::from_slice::<Value>(&result.stdout).map_err(|source| {
            Error::PlannerCacheProbeJson {
                substituter: substituter.to_owned(),
                output: chunk.first().cloned().unwrap_or_default(),
                reason: source.to_string(),
            }
        })?;
        for output in chunk {
            let Some(value) = values.get(output) else {
                return Err(Error::PlannerCacheProbeJson {
                    substituter: substituter.to_owned(),
                    output: output.clone(),
                    reason: "response did not contain the queried output".to_owned(),
                });
            };
            availability.insert(output.clone(), !value.is_null());
        }
        if !result.status.success() {
            return Err(Error::PlannerCacheProbeFailed {
                substituter: substituter.to_owned(),
                output: chunk.first().cloned().unwrap_or_default(),
                stderr: String::from_utf8_lossy(&result.stderr).trim().to_owned(),
            });
        }
    }
    Ok(availability)
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

    #[test]
    fn required_outputs_follow_graph_edges() {
        let root_json = r#"
        {"derivations": {
          "root.drv": {
            "system": "x86_64-linux",
            "outputs": {"out": {"path": "root"}},
            "inputs": {"drvs": {"dep.drv": {"outputs": ["dev"]}}}
          }
        }, "version": 4}
        "#;
        let graph_json = r#"
        {"derivations": {
          "root.drv": {
            "system": "x86_64-linux",
            "outputs": {"out": {"path": "root"}},
            "inputs": {"drvs": {"dep.drv": {"outputs": ["dev"]}}}
          },
          "dep.drv": {
            "system": "aarch64-linux",
            "outputs": {"out": {"path": "dep-out"}, "dev": {"path": "dep-dev"}},
            "inputs": {"drvs": {}}
          }
        }, "version": 4}
        "#;
        let roots = parse_graph(root_json).unwrap();
        let graph = parse_graph(graph_json).unwrap();
        let required = required_outputs(&roots, &graph);
        assert_eq!(required["root.drv"].iter().collect::<Vec<_>>(), [&"out"]);
        assert_eq!(required["dep.drv"].iter().collect::<Vec<_>>(), [&"dev"]);
    }

    #[test]
    fn parses_unique_derivation_paths_from_store_closure() {
        let paths =
            parse_drv_paths("/nix/store/a-source\nfoo.drv\n/nix/store/a.drv\n/nix/store/a.drv\n");
        assert_eq!(
            paths,
            BTreeSet::from([
                "/nix/store/a.drv".to_owned(),
                "/nix/store/foo.drv".to_owned(),
            ])
        );
    }

    #[test]
    fn union_probe_keeps_an_output_found_by_an_earlier_substituter() {
        let mut availability = BTreeMap::from([
            ("/nix/store/a".to_owned(), true),
            ("/nix/store/b".to_owned(), false),
        ]);
        let later_probe = BTreeMap::from([
            ("/nix/store/a".to_owned(), false),
            ("/nix/store/b".to_owned(), true),
        ]);

        merge_availability(&mut availability, later_probe);

        assert!(availability["/nix/store/a"]);
        assert!(availability["/nix/store/b"]);
    }
}
