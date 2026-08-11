//! Structured planning for cache-shaped Crossbow realizations.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, RealizationPolicy, Result};

/// The realization class assigned to one derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DerivationClass {
    /// Every requested output is already present in the local Nix store.
    LocalPresent,
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
    /// Full derivation name, when Nix reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Package name, when Nix reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pname: Option<String>,
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
    /// Derivations whose requested outputs are already in the local store.
    pub local_present: usize,
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

#[derive(Debug, Clone, Deserialize)]
struct DerivationGraph {
    derivations: BTreeMap<String, DerivationNode>,
}

#[derive(Debug, Clone, Deserialize)]
struct DerivationNode {
    system: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pname: Option<String>,
    #[serde(default)]
    env: DerivationEnv,
    outputs: BTreeMap<String, DerivationOutput>,
    #[serde(default)]
    inputs: DerivationInputs,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DerivationEnv {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pname: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DerivationInputs {
    #[serde(default)]
    drvs: BTreeMap<String, DerivationInput>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DerivationInput {
    #[serde(default)]
    outputs: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputAvailability {
    LocalPresent,
    HostSubstituted,
    Missing,
}

impl OutputAvailability {
    fn is_available(self) -> bool {
        !matches!(self, Self::Missing)
    }
}

impl DerivationPlan {
    /// Returns the most useful stable human label for this derivation.
    #[must_use]
    pub fn display_label(&self) -> String {
        match (&self.pname, &self.name) {
            (Some(pname), Some(name)) if pname != name => format!("{pname} ({name})"),
            (Some(pname), _) => pname.clone(),
            (None, Some(name)) => name.clone(),
            (None, None) => self
                .drv
                .rsplit('/')
                .next()
                .unwrap_or(&self.drv)
                .strip_suffix(".drv")
                .unwrap_or(&self.drv)
                .to_owned(),
        }
    }
}

/// Computes a structured plan from Nix's derivation closure.
///
/// `nix build --dry-run --json` reports only the requested installable, not its
/// complete action set. The requested roots are loaded first, then the planner
/// loads and probes child derivations only below unavailable nodes. This keeps
/// substituted and locally present outputs as realization leaves, matching
/// Nix's behavior of not realizing the inputs of a cache hit. Human stderr is
/// never parsed.
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

    let mut graph = roots.clone();
    let mut required = BTreeMap::<String, BTreeSet<String>>::new();
    for (key, node) in &graph.derivations {
        required
            .entry(key.clone())
            .or_default()
            .extend(node.outputs.keys().cloned());
    }

    let mut output_paths = BTreeMap::<String, Vec<String>>::new();
    let mut availability = BTreeMap::<String, OutputAvailability>::new();
    let mut frontier = BTreeMap::<String, BTreeSet<String>>::new();

    loop {
        let missing = required
            .keys()
            .filter(|key| !graph.derivations.contains_key(*key))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !missing.is_empty() {
            load_derivations(&mut graph, &missing)?;
        }

        let unresolved = required
            .keys()
            .filter(|key| !output_paths.contains_key(*key))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !unresolved.is_empty() {
            output_paths.extend(resolve_output_paths(
                &graph,
                &required,
                &unresolved,
                policy,
            )?);
        }

        let outputs_to_probe = output_paths
            .values()
            .flat_map(|paths| paths.iter())
            .filter(|path| !availability.contains_key(*path))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !outputs_to_probe.is_empty() {
            availability.extend(probe_outputs(
                substituters,
                &outputs_to_probe.into_iter().collect::<Vec<_>>(),
            )?);
        }

        if !expand_frontier(
            &graph,
            &mut required,
            &mut frontier,
            &output_paths,
            &availability,
        )? {
            break;
        }
    }

    let mut derivations = Vec::with_capacity(frontier.len());
    let mut counts = PlanCounts::default();
    for (key, names) in frontier {
        let Some(node) = graph.derivations.get(&key) else {
            continue;
        };
        if node.system == "builtin" {
            continue;
        }
        let outputs = names
            .iter()
            .filter_map(|name| output_path_for_name(&required, &output_paths, &key, name))
            .cloned()
            .collect::<Vec<_>>();
        let class = classify_derivation(
            &node.system,
            build_system,
            host_system,
            policy,
            &outputs,
            &availability,
        );
        let system = node.system.clone();

        match class {
            DerivationClass::LocalPresent => counts.local_present += 1,
            DerivationClass::HostSubstituted => counts.host_substituted += 1,
            DerivationClass::BuildLocal => counts.build_local += 1,
            DerivationClass::HostRemote => counts.host_remote += 1,
            DerivationClass::Unhandled => counts.unhandled += 1,
        }
        derivations.push(DerivationPlan {
            drv: store_path(&key),
            name: node
                .name
                .clone()
                .or_else(|| node.env.name.clone())
                .filter(|name| !name.is_empty()),
            pname: node
                .pname
                .clone()
                .or_else(|| node.env.pname.clone())
                .filter(|pname| !pname.is_empty()),
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

fn expand_frontier(
    graph: &DerivationGraph,
    required: &mut BTreeMap<String, BTreeSet<String>>,
    frontier: &mut BTreeMap<String, BTreeSet<String>>,
    output_paths: &BTreeMap<String, Vec<String>>,
    availability: &BTreeMap<String, OutputAvailability>,
) -> Result<bool> {
    let mut changed = false;
    for (key, names) in required.clone() {
        let entry = frontier.entry(key.clone()).or_default();
        if !names.iter().any(|name| entry.insert(name.clone())) {
            continue;
        }
        changed = true;

        let node = graph
            .derivations
            .get(&key)
            .ok_or_else(|| Error::PlannerMissingDerivation {
                drv: store_path(&key),
            })?;
        let available = node.system != "builtin"
            && !names.is_empty()
            && names.iter().all(|name| {
                output_path_for_name(required, output_paths, &key, name)
                    .and_then(|path| availability.get(path))
                    .is_some_and(|state| state.is_available())
            });
        if available {
            continue;
        }

        for (input_key, input) in &node.inputs.drvs {
            let entry = required.entry(derivation_key(input_key)).or_default();
            if input.outputs.iter().any(|name| entry.insert(name.clone())) {
                changed = true;
            }
        }
    }
    Ok(changed)
}

fn classify_derivation(
    system: &str,
    build_system: &str,
    host_system: &str,
    policy: &RealizationPolicy,
    outputs: &[String],
    availability: &BTreeMap<String, OutputAvailability>,
) -> DerivationClass {
    let local_present = !outputs.is_empty()
        && outputs
            .iter()
            .all(|output| availability.get(output) == Some(&OutputAvailability::LocalPresent));
    if local_present {
        return DerivationClass::LocalPresent;
    }

    let substitutable = !outputs.is_empty()
        && outputs.iter().all(|output| {
            availability
                .get(output)
                .is_some_and(|state| state.is_available())
        });
    if substitutable {
        return DerivationClass::HostSubstituted;
    }

    if system == build_system {
        DerivationClass::BuildLocal
    } else if system == host_system {
        match policy {
            RealizationPolicy::RemoteNative { builders } if !builders.is_empty() => {
                DerivationClass::HostRemote
            }
            RealizationPolicy::SubstituteOnly | RealizationPolicy::RemoteNative { builders: _ } => {
                DerivationClass::Unhandled
            }
        }
    } else {
        DerivationClass::Unhandled
    }
}

fn output_path_for_name<'a>(
    required: &BTreeMap<String, BTreeSet<String>>,
    output_paths: &'a BTreeMap<String, Vec<String>>,
    key: &str,
    name: &str,
) -> Option<&'a String> {
    let index = required
        .get(key)?
        .iter()
        .position(|required_name| required_name == name)?;
    output_paths.get(key)?.get(index)
}

fn parse_graph(value: &str) -> Result<DerivationGraph> {
    let graph: DerivationGraph =
        serde_json::from_str(value).map_err(|source| Error::PlannerJson { source })?;
    Ok(DerivationGraph {
        derivations: graph
            .derivations
            .into_iter()
            .map(|(key, node)| (derivation_key(&key), node))
            .collect(),
    })
}

fn load_derivations(graph: &mut DerivationGraph, keys: &BTreeSet<String>) -> Result<()> {
    for chunk in keys.iter().collect::<Vec<_>>().chunks(256) {
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
        for key in chunk {
            if !batch.derivations.contains_key(*key) {
                return Err(Error::PlannerMissingDerivation {
                    drv: store_path(key),
                });
            }
        }
        graph.derivations.extend(batch.derivations);
    }
    Ok(())
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

fn resolve_output_paths(
    graph: &DerivationGraph,
    required: &BTreeMap<String, BTreeSet<String>>,
    keys: &BTreeSet<String>,
    policy: &RealizationPolicy,
) -> Result<BTreeMap<String, Vec<String>>> {
    let mut resolved = BTreeMap::<(String, String), String>::new();
    let mut pending = Vec::<(String, String, String)>::new();

    for key in keys {
        let names = required
            .get(key)
            .ok_or_else(|| Error::PlannerMissingDerivation {
                drv: store_path(key),
            })?;
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
    for key in keys {
        let names = required
            .get(key)
            .ok_or_else(|| Error::PlannerMissingDerivation {
                drv: store_path(key),
            })?;
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

fn probe_outputs(
    substituters: &[String],
    outputs: &[String],
) -> Result<BTreeMap<String, OutputAvailability>> {
    if substituters.is_empty() {
        return Err(Error::PlannerNoSubstituters);
    }

    let mut availability = BTreeMap::new();
    let mut pending = Vec::new();
    for output in outputs {
        if Path::new(output).exists() {
            availability.insert(output.clone(), OutputAvailability::LocalPresent);
        } else {
            availability.insert(output.clone(), OutputAvailability::Missing);
            pending.push(output.clone());
        }
    }
    let mut queried = BTreeSet::new();

    for substituter in substituters {
        if !queried.insert(substituter) || pending.is_empty() {
            continue;
        }

        let probed = probe_substituter_outputs(substituter, &pending)?;
        merge_availability(&mut availability, probed);
        pending.retain(|output| {
            !availability
                .get(output)
                .is_some_and(|state| state.is_available())
        });
    }

    Ok(availability)
}

fn merge_availability(
    availability: &mut BTreeMap<String, OutputAvailability>,
    probed: BTreeMap<String, bool>,
) {
    for (output, present) in probed {
        if present && availability.get(&output) != Some(&OutputAvailability::LocalPresent) {
            availability.insert(output, OutputAvailability::HostSubstituted);
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
        assert!(json.contains("local_present"));
    }

    #[test]
    fn old_derivation_json_deserializes_without_labels() {
        let plan: DerivationPlan = serde_json::from_str(
            r#"{
                "drv": "/nix/store/example.drv",
                "outputs": ["/nix/store/example"],
                "system": "x86_64-linux",
                "class": "build-local"
            }"#,
        )
        .unwrap();

        assert_eq!(plan.name, None);
        assert_eq!(plan.pname, None);
    }

    #[test]
    fn parse_graph_normalizes_paths_and_reads_names() {
        let graph = parse_graph(
            r#"
            {"derivations": {
              "/nix/store/root.drv": {
                "name": "root-1.0",
                "pname": "root",
                "system": "x86_64-linux",
                "outputs": {"out": {"path": "root"}}
              }
            }, "version": 4}
            "#,
        )
        .unwrap();

        let node = &graph.derivations["root.drv"];
        assert_eq!(node.name.as_deref(), Some("root-1.0"));
        assert_eq!(node.pname.as_deref(), Some("root"));
    }

    #[test]
    fn frontier_expansion_prunes_inputs_of_substituted_nodes() {
        let graph = parse_graph(
            r#"
        {"derivations": {
          "root.drv": {
            "system": "x86_64-linux",
            "outputs": {"out": {"path": "root"}},
            "inputs": {"drvs": {"cached.drv": {"outputs": ["out"]}}}
          },
          "cached.drv": {
            "system": "aarch64-linux",
            "outputs": {"out": {"path": "cached"}},
            "inputs": {"drvs": {"missing.drv": {"outputs": ["out"]}}}
          },
          "missing.drv": {
            "system": "aarch64-linux",
            "outputs": {"out": {"path": "missing"}},
            "inputs": {"drvs": {}}
          }
        }, "version": 4}
        "#,
        )
        .unwrap();
        let mut required =
            BTreeMap::from([("root.drv".to_owned(), BTreeSet::from(["out".to_owned()]))]);
        let mut frontier = BTreeMap::new();
        let output_paths = BTreeMap::from([
            ("root.drv".to_owned(), vec!["/nix/store/root".to_owned()]),
            (
                "cached.drv".to_owned(),
                vec!["/nix/store/cached".to_owned()],
            ),
            (
                "missing.drv".to_owned(),
                vec!["/nix/store/missing".to_owned()],
            ),
        ]);
        let availability = BTreeMap::from([
            ("/nix/store/root".to_owned(), OutputAvailability::Missing),
            (
                "/nix/store/cached".to_owned(),
                OutputAvailability::HostSubstituted,
            ),
            ("/nix/store/missing".to_owned(), OutputAvailability::Missing),
        ]);

        assert!(
            expand_frontier(
                &graph,
                &mut required,
                &mut frontier,
                &output_paths,
                &availability,
            )
            .unwrap()
        );
        assert!(
            expand_frontier(
                &graph,
                &mut required,
                &mut frontier,
                &output_paths,
                &availability,
            )
            .unwrap()
        );

        assert!(frontier.contains_key("root.drv"));
        assert!(frontier.contains_key("cached.drv"));
        assert!(!required.contains_key("missing.drv"));
    }

    #[test]
    fn union_probe_keeps_an_output_found_by_an_earlier_substituter() {
        let mut availability = BTreeMap::from([
            ("/nix/store/a".to_owned(), OutputAvailability::LocalPresent),
            ("/nix/store/b".to_owned(), OutputAvailability::Missing),
        ]);
        let later_probe = BTreeMap::from([
            ("/nix/store/a".to_owned(), false),
            ("/nix/store/b".to_owned(), true),
        ]);

        merge_availability(&mut availability, later_probe);

        assert_eq!(
            availability["/nix/store/a"],
            OutputAvailability::LocalPresent
        );
        assert_eq!(
            availability["/nix/store/b"],
            OutputAvailability::HostSubstituted
        );
    }

    #[test]
    fn classification_distinguishes_local_and_remote_presence() {
        let local = BTreeMap::from([(
            "/nix/store/output".to_owned(),
            OutputAvailability::LocalPresent,
        )]);
        assert_eq!(
            classify_derivation(
                "x86_64-linux",
                "x86_64-linux",
                "aarch64-linux",
                &RealizationPolicy::SubstituteOnly,
                &["/nix/store/output".to_owned()],
                &local,
            ),
            DerivationClass::LocalPresent
        );

        let substituted = BTreeMap::from([(
            "/nix/store/output".to_owned(),
            OutputAvailability::HostSubstituted,
        )]);
        assert_eq!(
            classify_derivation(
                "x86_64-linux",
                "x86_64-linux",
                "aarch64-linux",
                &RealizationPolicy::SubstituteOnly,
                &["/nix/store/output".to_owned()],
                &substituted,
            ),
            DerivationClass::HostSubstituted
        );
    }

    #[test]
    fn display_label_prefers_package_name_and_keeps_derivation_name() {
        let plan = DerivationPlan {
            drv: "/nix/store/hash-canix-1.0.drv".to_owned(),
            name: Some("canix-1.0".to_owned()),
            pname: Some("canix".to_owned()),
            outputs: Vec::new(),
            system: "x86_64-linux".to_owned(),
            class: DerivationClass::BuildLocal,
        };

        assert_eq!(plan.display_label(), "canix (canix-1.0)");
    }
}
