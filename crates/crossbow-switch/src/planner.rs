//! Structured planning for cache-shaped Crossbow realizations.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Error, RealizationPolicy, Result};

/// Current version of the authoritative closure-plan schema.
pub const PLAN_SCHEMA_VERSION: u32 = 2;

type OutputPaths = BTreeMap<(String, String), String>;

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

/// What to do with a derivation that is not available from any substituter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MissRoute {
    /// Abort the plan; the derivation must substitute or the build is blocked.
    Fail,
    /// Build the derivation on the Crossbow build host.
    BuildLocal,
    /// Route the derivation to an explicitly declared native builder.
    RemoteNative,
}

/// A stable label describing which cache answered a probe for one output path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProbeResult {
    /// The output path is present in the named substituter.
    Present {
        /// Substituter store URL that served the path.
        substituter: String,
    },
    /// The output path is already present in the local Nix store.
    LocalPresent,
    /// No queried substituter reported the output path.
    Missing,
    /// At least one substituter could not be queried, and none reported a hit.
    /// This must not be treated as a cache miss.
    Indeterminate {
        /// Store URLs whose probe failed.
        errors: Vec<String>,
    },
    /// Probing was intentionally skipped for this known-local derivation.
    SkippedKnownLocal,
}

impl ProbeResult {
    fn is_available(&self) -> bool {
        matches!(self, Self::Present { .. } | Self::LocalPresent)
    }
}

/// Runtime routing policy binding a flake installable to a miss route.
///
/// Hints are exact: they bind to the derivation resolved from `installable`
/// against the frozen flake, never to an attribute name or package name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteHintSpec {
    /// Stable label surfaced in the plan and in diagnostics.
    pub label: String,
    /// Flake installable whose resolved derivation receives the hint.
    pub installable: String,
    /// Route applied when the hinted derivation misses every substituter.
    pub on_miss: MissRoute,
    /// Optional diagnostic provenance (e.g. the policy root that produced it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// When set, the hinted derivation's outputs are never probed and the
    /// `on_miss` route is assumed. Dependency outputs are still probed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_probe: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

/// How one derivation will be realized, decided at plan time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealizationRoute {
    /// Substitute from the named store.
    Substitute {
        /// Substituter store URL that serves the outputs.
        substituter: String,
    },
    /// The output is already present in the local store; nothing to do.
    AlreadyPresent,
    /// Build on the Crossbow build host.
    BuildLocal,
    /// Route to an explicitly declared native builder.
    RemoteNative {
        /// Nix builder specifications eligible for this derivation.
        builders: Vec<String>,
    },
    /// The plan is blocked for this derivation.
    Fail {
        /// Why the derivation cannot be realized.
        reason: String,
    },
}

/// Authoritative probe and route decision for one derivation output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputAction {
    /// Output name from the derivation.
    pub output: String,
    /// Expected output store path.
    pub path: String,
    /// Probe result used to select `route`.
    pub probe: ProbeResult,
    /// Exact route the executor must follow.
    pub route: RealizationRoute,
}

/// One dependency edge retained in the executable plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanDependency {
    /// Dependency derivation store path.
    pub drv: String,
    /// Outputs required from the dependency.
    pub outputs: Vec<String>,
}

/// Exact outputs requested from one root derivation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRoot {
    /// Root derivation store path.
    pub drv: String,
    /// Requested root output names, without implicit expansion to every output.
    pub outputs: Vec<String>,
}

impl Default for RealizationRoute {
    fn default() -> Self {
        Self::Fail {
            reason: String::new(),
        }
    }
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
    /// Requested output names (one per entry in `outputs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_names: Vec<String>,
    /// Output store paths reported by Nix.
    pub outputs: Vec<String>,
    /// Nix's build system for the derivation.
    pub system: String,
    /// Classification under the selected realization policy.
    pub class: DerivationClass,
    /// Concrete realization route selected at plan time.
    #[serde(default, skip_serializing_if = "is_default_route")]
    pub route: RealizationRoute,
    /// Hint label that produced this route, when a hint bound to the derivation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Outcome of probing the requested outputs against the substituters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeResult>,
    /// Authoritative independent actions for the requested outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<OutputAction>,
    /// Direct dependency edges retained from the realization frontier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<PlanDependency>,
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

fn is_default_route(route: &RealizationRoute) -> bool {
    *route
        == RealizationRoute::Fail {
            reason: String::new(),
        }
}

/// Counts for a structured closure plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanCounts {
    /// Outputs already available from the configured substituters.
    pub host_substituted: usize,
    /// Derivations whose requested outputs are already in the local store.
    #[serde(default)]
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
    /// Version of this executable plan schema.
    #[serde(default)]
    pub schema_version: u32,
    /// SHA-256 identifier over canonical plan content, excluding this field.
    #[serde(default)]
    pub plan_id: String,
    /// Exact toplevel installable requested during planning.
    #[serde(default)]
    pub toplevel_installable: String,
    /// Exact frozen flake installable that owns `toplevel_installable`.
    #[serde(default)]
    pub flake_installable: String,
    /// Build system running Crossbow.
    pub build_system: String,
    /// Target host system.
    pub host_system: String,
    /// Exact root outputs requested by the installable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<PlanRoot>,
    /// Derivations in the realized closure graph.
    pub derivations: Vec<DerivationPlan>,
    /// Aggregate class counts.
    pub counts: PlanCounts,
}

impl ClosurePlan {
    /// Computes the stable identifier for this plan's canonical content.
    pub fn canonical_plan_id(&self) -> Result<String> {
        #[derive(Serialize)]
        struct CanonicalPlan<'a> {
            schema_version: u32,
            toplevel_installable: &'a str,
            flake_installable: &'a str,
            build_system: &'a str,
            host_system: &'a str,
            roots: &'a [PlanRoot],
            derivations: &'a [DerivationPlan],
            counts: &'a PlanCounts,
        }

        let bytes = serde_json::to_vec(&CanonicalPlan {
            schema_version: self.schema_version,
            toplevel_installable: &self.toplevel_installable,
            flake_installable: &self.flake_installable,
            build_system: &self.build_system,
            host_system: &self.host_system,
            roots: &self.roots,
            derivations: &self.derivations,
            counts: &self.counts,
        })
        .map_err(|source| Error::PlannerJson { source })?;
        Ok(format!("sha256-{:x}", Sha256::digest(bytes)))
    }

    fn seal(mut self) -> Result<Self> {
        self.plan_id = self.canonical_plan_id()?;
        Ok(self)
    }
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
    plan_closure_with_hints(attr, build_system, host_system, substituters, policy, &[])
}

/// Computes a structured plan using the union of the supplied substituters and
/// explicit runtime route hints.
///
/// Hints bind to the exact derivation resolved from each hint's `installable`,
/// selected the same way the toplevel is resolved (frozen flake attributes).
/// `skip_probe` derivations are never queried against substituters; their
/// `on_miss` route is assumed. In all cases the dependency subtree shaped by
/// the realization frontier is still probed normally.
pub fn plan_closure_with_hints(
    attr: &str,
    build_system: &str,
    host_system: &str,
    substituters: &[String],
    policy: &RealizationPolicy,
    hints: &[RouteHintSpec],
) -> Result<ClosurePlan> {
    let roots = resolve_roots(attr, substituters, policy)?;
    let root_args = ["derivation", "show", "--no-pretty", attr]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let root_graph = parse_graph(&run_nix(&root_args)?)?;
    if roots.is_empty() {
        return ClosurePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            toplevel_installable: attr.to_owned(),
            flake_installable: flake_installable_for_toplevel(attr),
            build_system: build_system.to_owned(),
            host_system: host_system.to_owned(),
            roots,
            derivations: Vec::new(),
            counts: PlanCounts::default(),
        }
        .seal();
    }

    let hint_drvs = resolve_hint_drvs(hints)?;
    let mut graph = root_graph;
    let mut required = BTreeMap::<String, BTreeSet<String>>::new();
    for root in &roots {
        required
            .entry(derivation_key(&root.drv))
            .or_default()
            .extend(root.outputs.iter().cloned());
    }

    let mut output_paths = OutputPaths::new();
    let mut availability = BTreeMap::<String, ProbeResult>::new();
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

        let unresolved = unresolved_outputs(&required, &output_paths);
        if !unresolved.is_empty() {
            output_paths.extend(resolve_output_paths(
                &graph,
                &unresolved,
                substituters,
                policy,
            )?);
        }

        let outputs_to_probe = output_paths
            .values()
            .filter(|path| !availability.contains_key(*path))
            .filter(|path| {
                output_paths.iter().any(|((key, _), candidate)| {
                    candidate == *path
                        && graph
                            .derivations
                            .get(key)
                            .is_some_and(|node| node.system != "builtin")
                })
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        // `skip_probe` hint outputs are known local roots: never queried
        // against substituters, their on-miss route is assumed instead.
        let outputs_to_probe = outputs_to_probe
            .difference(&hint_skip_probe_paths(&hint_drvs, &output_paths))
            .cloned()
            .collect::<Vec<_>>();
        if !outputs_to_probe.is_empty() {
            availability.extend(probe_outputs(substituters, &outputs_to_probe)?);
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
        let output_names = names.iter().cloned().collect::<Vec<_>>();
        let outputs = output_names
            .iter()
            .map(|name| output_path_for_name(&output_paths, &key, name))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let system = node.system.clone();
        let actions = output_names
            .iter()
            .map(|output| {
                let path = output_path_for_name(&output_paths, &key, output)?;
                let probe = if system == "builtin"
                    || hint_drvs.get(&key).is_some_and(|hint| hint.skip_probe)
                {
                    ProbeResult::SkippedKnownLocal
                } else {
                    availability
                        .get(path)
                        .cloned()
                        .unwrap_or_else(|| ProbeResult::Indeterminate {
                            errors: vec!["output was not probed".to_owned()],
                        })
                };
                let route = select_route(
                    &key,
                    &probe,
                    hints,
                    &hint_drvs,
                    &system,
                    build_system,
                    host_system,
                    policy,
                );
                Ok(OutputAction {
                    output: output.clone(),
                    path: path.clone(),
                    probe,
                    route,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let probe = summarize_probes(&actions);
        let route = summarize_routes(&actions);
        let class = route_class(&route);

        match class {
            DerivationClass::LocalPresent => counts.local_present += 1,
            DerivationClass::HostSubstituted => counts.host_substituted += 1,
            DerivationClass::BuildLocal => counts.build_local += 1,
            DerivationClass::HostRemote => counts.host_remote += 1,
            DerivationClass::Unhandled => counts.unhandled += 1,
        }
        let hint = hint_drvs.get(&key).map(|spec| spec.label.clone());
        let dependencies = node
            .inputs
            .drvs
            .iter()
            .filter_map(|(input_key, input)| {
                let drv = derivation_key(input_key);
                let requested = required.get(&drv)?;
                let outputs = input
                    .outputs
                    .iter()
                    .filter(|output| requested.contains(*output))
                    .cloned()
                    .collect::<Vec<_>>();
                (!outputs.is_empty()).then(|| PlanDependency {
                    drv: store_path(&drv),
                    outputs,
                })
            })
            .collect();
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
            output_names,
            outputs: outputs.clone(),
            system,
            class,
            route,
            hint,
            probe: Some(probe),
            actions,
            dependencies,
        });
    }

    derivations = dependency_first(derivations, &roots);

    ClosurePlan {
        schema_version: PLAN_SCHEMA_VERSION,
        plan_id: String::new(),
        toplevel_installable: attr.to_owned(),
        flake_installable: flake_installable_for_toplevel(attr),
        build_system: build_system.to_owned(),
        host_system: host_system.to_owned(),
        roots,
        derivations,
        counts,
    }
    .seal()
}

fn flake_installable_for_toplevel(toplevel: &str) -> String {
    let Some((source, fragment)) = toplevel.split_once('#') else {
        return String::new();
    };
    fragment
        .strip_prefix("nixosConfigurations.")
        .and_then(|fragment| fragment.strip_suffix(".config.system.build.toplevel"))
        .map_or_else(String::new, |configuration| {
            format!("{source}#{configuration}")
        })
}

fn resolve_roots(
    attr: &str,
    substituters: &[String],
    policy: &RealizationPolicy,
) -> Result<Vec<PlanRoot>> {
    let args = dry_run_args(&[attr.to_owned()], substituters, policy)?;
    let output = run_command_silent("nix", &args, format!("nix build --dry-run --json {attr}"))?;
    parse_roots(&output)
}

fn unresolved_outputs(
    required: &BTreeMap<String, BTreeSet<String>>,
    output_paths: &OutputPaths,
) -> BTreeSet<(String, String)> {
    required
        .iter()
        .flat_map(|(key, names)| names.iter().map(|name| (key.clone(), name.clone())))
        .filter(|pair| !output_paths.contains_key(pair))
        .collect()
}

fn parse_roots(output: &str) -> Result<Vec<PlanRoot>> {
    let entries: Vec<OutputEntry> =
        serde_json::from_str(output).map_err(|source| Error::PlannerJson { source })?;
    Ok(entries
        .into_iter()
        .map(|entry| PlanRoot {
            drv: store_path(&entry.drv_path),
            outputs: entry.outputs.into_keys().collect(),
        })
        .collect())
}

fn summarize_probes(actions: &[OutputAction]) -> ProbeResult {
    let errors = actions
        .iter()
        .filter_map(|action| match &action.probe {
            ProbeResult::Indeterminate { errors } => Some(errors.as_slice()),
            _ => None,
        })
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return ProbeResult::Indeterminate { errors };
    }
    if actions
        .iter()
        .any(|action| matches!(action.probe, ProbeResult::Missing))
    {
        return ProbeResult::Missing;
    }
    if actions
        .iter()
        .all(|action| matches!(action.probe, ProbeResult::LocalPresent))
    {
        return ProbeResult::LocalPresent;
    }
    if actions
        .iter()
        .all(|action| matches!(action.probe, ProbeResult::SkippedKnownLocal))
    {
        return ProbeResult::SkippedKnownLocal;
    }
    actions
        .iter()
        .find_map(|action| match &action.probe {
            ProbeResult::Present { substituter } => Some(ProbeResult::Present {
                substituter: substituter.clone(),
            }),
            _ => None,
        })
        .unwrap_or_else(|| ProbeResult::Indeterminate {
            errors: vec!["no output actions".to_owned()],
        })
}

fn summarize_routes(actions: &[OutputAction]) -> RealizationRoute {
    actions
        .iter()
        .map(|action| &action.route)
        .min_by_key(|route| match route {
            RealizationRoute::Fail { .. } => 0,
            RealizationRoute::RemoteNative { .. } => 1,
            RealizationRoute::BuildLocal => 2,
            RealizationRoute::Substitute { .. } => 3,
            RealizationRoute::AlreadyPresent => 4,
        })
        .cloned()
        .unwrap_or_else(|| RealizationRoute::Fail {
            reason: "derivation has no output actions".to_owned(),
        })
}

fn dependency_first(mut plans: Vec<DerivationPlan>, roots: &[PlanRoot]) -> Vec<DerivationPlan> {
    let mut by_drv = plans
        .drain(..)
        .map(|plan| (plan.drv.clone(), plan))
        .collect::<BTreeMap<_, _>>();
    let mut ordered = Vec::with_capacity(by_drv.len());
    let mut visited = BTreeSet::new();

    fn visit(
        drv: &str,
        by_drv: &mut BTreeMap<String, DerivationPlan>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<DerivationPlan>,
    ) {
        if !visited.insert(drv.to_owned()) {
            return;
        }
        let dependencies = by_drv
            .get(drv)
            .map(|plan| plan.dependencies.clone())
            .unwrap_or_default();
        for dependency in dependencies {
            visit(&dependency.drv, by_drv, visited, ordered);
        }
        if let Some(plan) = by_drv.remove(drv) {
            ordered.push(plan);
        }
    }

    for root in roots {
        visit(&root.drv, &mut by_drv, &mut visited, &mut ordered);
    }
    let remaining = by_drv.keys().cloned().collect::<Vec<_>>();
    for drv in remaining {
        visit(&drv, &mut by_drv, &mut visited, &mut ordered);
    }
    ordered
}

/// Resolves each route hint's `installable` to the exact derivation it names.
///
/// Hints bind to derivation store paths, not attribute names, so a hint always
/// targets the same derivation regardless of how the frozen flake is reached.
fn resolve_hint_drvs(hints: &[RouteHintSpec]) -> Result<BTreeMap<String, RouteHintSpec>> {
    let mut resolved = BTreeMap::new();
    for hint in hints {
        let args = ["derivation", "show", "--no-pretty", &hint.installable]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let graph = parse_graph(&run_nix(&args)?)?;
        let Some(key) = graph.derivations.keys().next() else {
            return Err(Error::PlannerMissingDerivation {
                drv: hint.installable.clone(),
            });
        };
        resolved.insert(derivation_key(key), hint.clone());
    }
    Ok(resolved)
}

/// Output paths that a `skip_probe` hint removes from substituter probing.
fn hint_skip_probe_paths(
    hint_drvs: &BTreeMap<String, RouteHintSpec>,
    output_paths: &OutputPaths,
) -> BTreeSet<String> {
    hint_drvs
        .iter()
        .filter(|(_, hint)| hint.skip_probe)
        .flat_map(|(key, _)| {
            output_paths
                .iter()
                .filter(move |((drv, _), _)| drv == key)
                .map(|(_, path)| path.clone())
        })
        .collect()
}

/// Combines per-output probe answers into one derivation-level result.
#[cfg(test)]
fn derive_probe_result(
    key: &str,
    hint_drvs: &BTreeMap<String, RouteHintSpec>,
    outputs: &[String],
    availability: &BTreeMap<String, ProbeResult>,
) -> ProbeResult {
    if hint_drvs.get(key).is_some_and(|hint| hint.skip_probe) {
        return ProbeResult::SkippedKnownLocal;
    }
    let mut any_indeterminate = Vec::new();
    let mut any_present = Option::<String>::None;
    let mut all_local = !outputs.is_empty();
    for output in outputs {
        match availability.get(output) {
            Some(ProbeResult::Present { substituter }) => {
                all_local = false;
                any_present.get_or_insert_with(|| substituter.clone());
            }
            Some(ProbeResult::LocalPresent) => {}
            Some(ProbeResult::Indeterminate { errors }) => {
                all_local = false;
                any_indeterminate.extend(errors.clone());
            }
            Some(ProbeResult::Missing | ProbeResult::SkippedKnownLocal) => {
                all_local = false;
            }
            None => {
                all_local = false;
            }
        }
    }
    if all_local {
        return ProbeResult::LocalPresent;
    }
    if let Some(substituter) = any_present {
        return ProbeResult::Present { substituter };
    }
    if !any_indeterminate.is_empty() {
        return ProbeResult::Indeterminate {
            errors: any_indeterminate,
        };
    }
    ProbeResult::Missing
}

/// Chooses the realization route for one frontier derivation.
#[allow(clippy::too_many_arguments)]
fn select_route(
    key: &str,
    probe: &ProbeResult,
    hints: &[RouteHintSpec],
    hint_drvs: &BTreeMap<String, RouteHintSpec>,
    system: &str,
    build_system: &str,
    host_system: &str,
    policy: &RealizationPolicy,
) -> RealizationRoute {
    let hint = hint_drvs.get(key);
    if system == "builtin" {
        return RealizationRoute::BuildLocal;
    }
    match probe {
        ProbeResult::Present { substituter } => RealizationRoute::Substitute {
            substituter: substituter.clone(),
        },
        ProbeResult::LocalPresent => RealizationRoute::AlreadyPresent,
        ProbeResult::SkippedKnownLocal => route_for_miss(hint, system, build_system, policy),
        ProbeResult::Indeterminate { errors } => RealizationRoute::Fail {
            reason: format!("output probe indeterminate: {}", errors.join(", ")),
        },
        ProbeResult::Missing => {
            if let Some(hint) = hint {
                return route_for_hint(hint, hints, system, build_system, policy);
            }
            if system == build_system {
                return RealizationRoute::BuildLocal;
            }
            if system == host_system {
                return match policy {
                    RealizationPolicy::RemoteNative { builders } if !builders.is_empty() => {
                        RealizationRoute::RemoteNative {
                            builders: builders.clone(),
                        }
                    }
                    RealizationPolicy::SubstituteOnly
                    | RealizationPolicy::RemoteNative { builders: _ } => RealizationRoute::Fail {
                        reason: "host-system miss with no eligible native builder".to_owned(),
                    },
                };
            }
            RealizationRoute::Fail {
                reason: format!("system `{system}` is neither build nor host"),
            }
        }
    }
}

fn route_for_hint(
    hint: &RouteHintSpec,
    _hints: &[RouteHintSpec],
    system: &str,
    build_system: &str,
    policy: &RealizationPolicy,
) -> RealizationRoute {
    match hint.on_miss {
        MissRoute::BuildLocal if system == build_system => RealizationRoute::BuildLocal,
        MissRoute::BuildLocal => RealizationRoute::Fail {
            reason: format!(
                "hint `{}` requests build-local but derivation runs on `{system}`",
                hint.label
            ),
        },
        MissRoute::RemoteNative => match policy {
            RealizationPolicy::RemoteNative { builders } if !builders.is_empty() => {
                RealizationRoute::RemoteNative {
                    builders: builders.clone(),
                }
            }
            RealizationPolicy::SubstituteOnly | RealizationPolicy::RemoteNative { builders: _ } => {
                RealizationRoute::Fail {
                    reason: format!(
                        "hint `{}` requests remote-native but no eligible builder is declared",
                        hint.label
                    ),
                }
            }
        },
        MissRoute::Fail => RealizationRoute::Fail {
            reason: format!("hint `{}` requires substitution", hint.label),
        },
    }
}

/// Applies the known-local (skip-probe) `on_miss` without probing.
fn route_for_miss(
    hint: Option<&RouteHintSpec>,
    system: &str,
    build_system: &str,
    policy: &RealizationPolicy,
) -> RealizationRoute {
    match hint {
        Some(hint) => route_for_hint(hint, &[], system, build_system, policy),
        None => {
            if system == build_system {
                RealizationRoute::BuildLocal
            } else {
                RealizationRoute::Fail {
                    reason: "known-local derivation not on the build system".to_owned(),
                }
            }
        }
    }
}

/// Maps a realization route back to the legacy class used by existing gates.
fn route_class(route: &RealizationRoute) -> DerivationClass {
    match route {
        RealizationRoute::Substitute { .. } => DerivationClass::HostSubstituted,
        RealizationRoute::AlreadyPresent => DerivationClass::LocalPresent,
        RealizationRoute::BuildLocal => DerivationClass::BuildLocal,
        RealizationRoute::RemoteNative { .. } => DerivationClass::HostRemote,
        RealizationRoute::Fail { .. } => DerivationClass::Unhandled,
    }
}

/// Expands the realization frontier to the inputs of unavailable nodes.
///
/// A derivation whose requested outputs are all available from the configured
/// substituters or the local store is a leaf in the realization graph: its
/// inputs are only build inputs for the cached result and must not be treated
/// as additional misses. The frontier can only be entered once per derivation,
/// so the loop terminates even when the graph contains cycles.
fn expand_frontier(
    graph: &DerivationGraph,
    required: &mut BTreeMap<String, BTreeSet<String>>,
    frontier: &mut BTreeMap<String, BTreeSet<String>>,
    output_paths: &OutputPaths,
    availability: &BTreeMap<String, ProbeResult>,
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
        let available = !names.is_empty()
            && node.system != "builtin"
            && names.iter().all(|name| {
                output_paths
                    .get(&(key.clone(), name.clone()))
                    .and_then(|path| availability.get(path))
                    .is_some_and(ProbeResult::is_available)
            });
        if available {
            continue;
        }

        for (input_key, input) in &node.inputs.drvs {
            let entry = required.entry(derivation_key(input_key)).or_default();
            for name in &input.outputs {
                changed |= entry.insert(name.clone());
            }
        }
    }
    Ok(changed)
}

fn output_path_for_name<'a>(
    output_paths: &'a OutputPaths,
    key: &str,
    name: &str,
) -> Result<&'a String> {
    output_paths
        .get(&(key.to_owned(), name.to_owned()))
        .ok_or_else(|| Error::PlannerMissingOutput {
            drv: store_path(key),
            output: name.to_owned(),
        })
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
        args.extend(chunk.iter().map(|path| store_path(path)));
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
    outputs: &BTreeSet<(String, String)>,
    substituters: &[String],
    policy: &RealizationPolicy,
) -> Result<OutputPaths> {
    let mut resolved = OutputPaths::new();
    let mut pending = Vec::<(String, String, String)>::new();

    for (key, name) in outputs {
        let Some(node) = graph.derivations.get(key) else {
            return Err(Error::PlannerMissingDerivation {
                drv: store_path(key),
            });
        };
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

    for chunk in pending.chunks(256) {
        let specs = chunk
            .iter()
            .map(|(_, _, spec)| spec.clone())
            .collect::<Vec<_>>();
        let args = dry_run_args(&specs, substituters, policy)?;
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

    for (key, name) in outputs {
        if !resolved.contains_key(&(key.clone(), name.clone())) {
            return Err(Error::PlannerMissingOutput {
                drv: store_path(key),
                output: name.clone(),
            });
        }
    }
    Ok(resolved)
}

fn dry_run_args(
    installables: &[String],
    substituters: &[String],
    policy: &RealizationPolicy,
) -> Result<Vec<String>> {
    let mut args = ["build", "--dry-run", "--json", "--no-link"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    args.extend_from_slice(installables);
    args.extend(planner_realization_flags(substituters, policy)?);
    Ok(args)
}

fn planner_realization_flags(
    substituters: &[String],
    policy: &RealizationPolicy,
) -> Result<Vec<String>> {
    let mut flags = crate::realization_nix_flags(policy)?;
    flags.extend([
        "--option".to_owned(),
        "substituters".to_owned(),
        substituters.join(" "),
    ]);
    Ok(flags)
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

/// Probes every requested output against the substituters, in parallel.
///
/// Outputs already present in the local store are marked `LocalPresent`
/// without being queried. One scoped thread per unique substituter probes the
/// remaining outputs in bounded batches, so latency is bounded by the slowest
/// store instead of their sum. A store that errors contributes `Indeterminate`
/// results: the errored store cannot be ruled out as a source, so its outputs
/// must not be treated as cache misses.
fn probe_outputs(
    substituters: &[String],
    outputs: &[String],
) -> Result<BTreeMap<String, ProbeResult>> {
    if substituters.is_empty() {
        return Err(Error::PlannerNoSubstituters);
    }
    if outputs.is_empty() {
        return Ok(BTreeMap::new());
    }

    let local = probe_substituter_outputs("daemon", outputs);
    let mut availability = BTreeMap::new();
    let pending = match &local {
        Ok(probed) => outputs
            .iter()
            .filter(|output| !probed.get(*output).copied().unwrap_or(false))
            .cloned()
            .collect::<Vec<_>>(),
        Err(_) => outputs.to_vec(),
    };
    if let Ok(probed) = &local {
        availability.extend(
            probed
                .iter()
                .filter(|(_, present)| **present)
                .map(|(output, _)| (output.clone(), ProbeResult::LocalPresent)),
        );
    }

    let mut seen = BTreeSet::new();
    let unique = substituters
        .iter()
        .filter(|substituter| seen.insert((*substituter).clone()))
        .cloned()
        .collect::<Vec<_>>();

    let mut probes = Vec::with_capacity(unique.len());

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(unique.len());
        for substituter in &unique {
            let store = substituter.clone();
            let own = pending.clone();
            handles.push((
                substituter.clone(),
                scope.spawn(move || probe_substituter_outputs(&store, &own)),
            ));
        }
        for (substituter, handle) in handles {
            let result = handle.join().unwrap_or_else(|_| {
                Err(Error::PlannerCacheProbe {
                    substituter: substituter.clone(),
                    output: pending.first().cloned().unwrap_or_default(),
                    source: std::io::Error::other("probe thread panicked"),
                })
            });
            probes.push((substituter, result));
        }
    });

    availability.extend(merge_store_probe_results(&pending, &probes, local.is_err()));
    // Stores that errored may have answered none of the outputs; make sure
    // every requested output has an entry.
    for output in &pending {
        availability
            .entry(output.clone())
            .or_insert(ProbeResult::Missing);
    }
    Ok(availability)
}

/// Merges per-store probe maps into one output-to-result map.
#[cfg(test)]
fn merge_store_probes(
    stores: &[&str],
    probes: &[BTreeMap<String, bool>],
) -> BTreeMap<String, ProbeResult> {
    merge_store_probes_with_errors(stores, probes, &[])
}

/// Merges per-store probe maps and marks store failures as indeterminate.
#[cfg(test)]
fn merge_store_probes_with_errors(
    stores: &[&str],
    probes: &[BTreeMap<String, bool>],
    failed_stores: &[&str],
) -> BTreeMap<String, ProbeResult> {
    let mut present = BTreeMap::<String, String>::new();
    for (store, probe) in stores.iter().zip(probes) {
        for (output, is_present) in probe {
            if *is_present {
                present
                    .entry(output.clone())
                    .or_insert_with(|| (*store).to_owned());
            }
        }
    }
    let mut result = BTreeMap::<String, ProbeResult>::new();
    // Every output asked of any store is a candidate. A confirmed hit wins;
    // a clean all-miss is Missing; any store failure turns misses into
    // Indeterminate because the errored store could have served the path.
    let mut outputs = BTreeSet::<&str>::new();
    for probe in probes {
        outputs.extend(probe.keys().map(String::as_str));
    }
    let errors = failed_stores
        .iter()
        .map(|s| (*s).to_owned())
        .collect::<Vec<_>>();
    for output in outputs {
        if let Some(substituter) = present.get(output) {
            result.insert(
                output.to_owned(),
                ProbeResult::Present {
                    substituter: substituter.clone(),
                },
            );
        } else if errors.is_empty() {
            result.insert(output.to_owned(), ProbeResult::Missing);
        } else {
            result.insert(
                output.to_owned(),
                ProbeResult::Indeterminate {
                    errors: errors.clone(),
                },
            );
        }
    }
    result
}

fn merge_store_probe_results(
    outputs: &[String],
    probes: &[(String, Result<BTreeMap<String, bool>>)],
    local_probe_failed: bool,
) -> BTreeMap<String, ProbeResult> {
    let mut result = BTreeMap::new();
    for output in outputs {
        let hit = probes.iter().find_map(|(store, probe)| {
            probe
                .as_ref()
                .ok()
                .and_then(|values| values.get(output))
                .copied()
                .unwrap_or(false)
                .then(|| store.clone())
        });
        if let Some(substituter) = hit {
            result.insert(output.clone(), ProbeResult::Present { substituter });
            continue;
        }
        let mut errors = probes
            .iter()
            .filter(|(_, probe)| probe.is_err())
            .map(|(store, _)| store.clone())
            .collect::<Vec<_>>();
        if local_probe_failed {
            errors.insert(0, "daemon".to_owned());
        }
        result.insert(
            output.clone(),
            if errors.is_empty() {
                ProbeResult::Missing
            } else {
                ProbeResult::Indeterminate { errors }
            },
        );
    }
    result
}

/// Queries one substituter for every output path, in bounded batches.
fn probe_substituter_outputs(
    substituter: &str,
    outputs: &[String],
) -> Result<BTreeMap<String, bool>> {
    let ping = Command::new("nix")
        .args(["store", "ping", "--store", substituter])
        .output()
        .map_err(|source| Error::PlannerCacheProbe {
            substituter: substituter.to_owned(),
            output: outputs.first().cloned().unwrap_or_default(),
            source,
        })?;
    if !ping.status.success() {
        return Err(Error::PlannerCacheProbeFailed {
            substituter: substituter.to_owned(),
            output: outputs.first().cloned().unwrap_or_default(),
            stderr: String::from_utf8_lossy(&ping.stderr).trim().to_owned(),
        });
    }
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
        // `nix path-info` exits non-zero for ordinary misses while still
        // returning a complete JSON map with null values. Complete JSON is the
        // authoritative per-path answer; malformed or incomplete JSON fails.
    }
    Ok(availability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_machine_readable_plan() {
        let plan = ClosurePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            toplevel_installable: "/nix/store/source#top".to_owned(),
            flake_installable: "/nix/store/source#host".to_owned(),
            build_system: "x86_64-linux".into(),
            host_system: "aarch64-linux".into(),
            roots: vec![],
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

        let old: ClosurePlan = serde_json::from_str(
            r#"{
                "build_system": "x86_64-linux",
                "host_system": "aarch64-linux",
                "derivations": [],
                "counts": {
                    "host_substituted": 1,
                    "build_local": 2,
                    "host_remote": 0,
                    "unhandled": 0
                }
            }"#,
        )
        .unwrap();
        assert_eq!(old.counts.local_present, 0);
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
    fn root_parser_preserves_only_requested_outputs() {
        let roots = parse_roots(
            r#"[{"drvPath":"/nix/store/root.drv","outputs":{"dev":"/nix/store/root-dev"}}]"#,
        )
        .unwrap();
        assert_eq!(
            roots,
            vec![PlanRoot {
                drv: "/nix/store/root.drv".to_owned(),
                outputs: vec!["dev".to_owned()]
            }]
        );
    }

    #[test]
    fn real_nix_missing_json_and_selected_output_are_machine_readable() {
        if Command::new("nix").arg("--version").output().is_err() {
            return;
        }
        let missing = "/nix/store/00000000000000000000000000000000-crossbow-invalid";
        let output = Command::new("nix")
            .args([
                "path-info",
                "--json",
                "--json-format",
                "1",
                "--store",
                "daemon",
                missing,
            ])
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(json.get(missing).unwrap().is_null());

        let output = Command::new("nix")
            .args([
                "build",
                "--dry-run",
                "--json",
                "--no-link",
                "--impure",
                "--expr",
                r#"derivation { name = "crossbow-selected-output-test"; system = builtins.currentSystem; builder = "/bin/sh"; args = [ "-c" "mkdir -p $out $dev" ]; outputs = [ "out" "dev" ]; }"#,
                "^dev",
                "--option",
                "substituters",
                "",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let roots = parse_roots(&String::from_utf8(output.stdout).unwrap()).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].outputs, ["dev"]);
    }

    #[test]
    fn later_required_output_of_known_derivation_is_unresolved() {
        let mut required =
            BTreeMap::from([("shared.drv".to_owned(), BTreeSet::from(["out".to_owned()]))]);
        let paths = BTreeMap::from([(
            ("shared.drv".to_owned(), "out".to_owned()),
            "/nix/store/shared".to_owned(),
        )]);
        assert!(unresolved_outputs(&required, &paths).is_empty());

        required
            .get_mut("shared.drv")
            .unwrap()
            .insert("dev".to_owned());
        assert_eq!(
            unresolved_outputs(&required, &paths),
            BTreeSet::from([("shared.drv".to_owned(), "dev".to_owned())])
        );
    }

    #[test]
    fn frontier_expansion_keeps_every_required_dependency_output() {
        let graph = parse_graph(
            r#"{"derivations": {
              "root.drv": {"system":"x86_64-linux","outputs":{"out":{"path":"root"}},"inputs":{"drvs":{"dependency.drv":{"outputs":["man","out"]}}}},
              "dependency.drv": {"system":"aarch64-linux","outputs":{"man":{"path":"dependency-man"},"out":{"path":"dependency"}},"inputs":{"drvs":{}}}
            },"version":4}"#,
        )
        .unwrap();
        let mut required =
            BTreeMap::from([("root.drv".to_owned(), BTreeSet::from(["out".to_owned()]))]);
        let mut frontier = BTreeMap::new();
        let paths = BTreeMap::from([(
            ("root.drv".to_owned(), "out".to_owned()),
            "/nix/store/root".to_owned(),
        )]);
        let availability = BTreeMap::from([("/nix/store/root".to_owned(), ProbeResult::Missing)]);

        expand_frontier(&graph, &mut required, &mut frontier, &paths, &availability).unwrap();

        assert_eq!(
            required["dependency.drv"],
            BTreeSet::from(["man".to_owned(), "out".to_owned()])
        );
    }

    #[test]
    fn later_frontier_expansion_resolves_new_output_of_known_derivation() {
        let graph = parse_graph(
            r#"{"derivations": {
              "root.drv": {"system":"x86_64-linux","outputs":{"out":{"path":"root"}},"inputs":{"drvs":{"first.drv":{"outputs":["out"]}}}},
              "first.drv": {"system":"x86_64-linux","outputs":{"out":{"path":"first"}},"inputs":{"drvs":{"shared.drv":{"outputs":["out"]},"later.drv":{"outputs":["out"]}}}},
              "later.drv": {"system":"x86_64-linux","outputs":{"out":{"path":"later"}},"inputs":{"drvs":{"shared.drv":{"outputs":["dev"]}}}},
              "shared.drv": {"system":"x86_64-linux","outputs":{"out":{"path":"shared"},"dev":{"path":"shared-dev"}},"inputs":{"drvs":{}}}
            },"version":4}"#,
        )
        .unwrap();
        let mut required =
            BTreeMap::from([("root.drv".to_owned(), BTreeSet::from(["out".to_owned()]))]);
        let mut frontier = BTreeMap::new();
        let mut paths = BTreeMap::from([
            (
                ("root.drv".to_owned(), "out".to_owned()),
                "/nix/store/root".to_owned(),
            ),
            (
                ("first.drv".to_owned(), "out".to_owned()),
                "/nix/store/first".to_owned(),
            ),
            (
                ("later.drv".to_owned(), "out".to_owned()),
                "/nix/store/later".to_owned(),
            ),
            (
                ("shared.drv".to_owned(), "out".to_owned()),
                "/nix/store/shared".to_owned(),
            ),
        ]);
        let availability = paths
            .values()
            .map(|path| (path.clone(), ProbeResult::Missing))
            .collect();

        for _ in 0..3 {
            expand_frontier(&graph, &mut required, &mut frontier, &paths, &availability).unwrap();
        }
        let unresolved = unresolved_outputs(&required, &paths);
        assert_eq!(
            unresolved,
            BTreeSet::from([("shared.drv".to_owned(), "dev".to_owned())])
        );
        paths.extend(
            resolve_output_paths(
                &graph,
                &unresolved,
                &["https://cache".to_owned()],
                &RealizationPolicy::SubstituteOnly,
            )
            .unwrap(),
        );
        assert_eq!(
            paths[&("shared.drv".to_owned(), "dev".to_owned())],
            "/nix/store/shared-dev"
        );
    }

    #[test]
    fn planner_dry_runs_pin_only_ordered_substituters() {
        let args = dry_run_args(
            &["/nix/store/root.drv^out".to_owned()],
            &["https://private".to_owned(), "https://public".to_owned()],
            &RealizationPolicy::SubstituteOnly,
        )
        .unwrap();
        assert!(args.windows(3).any(|args| {
            args == ["--option", "substituters", "https://private https://public"]
        }));
        assert_eq!(
            args.iter()
                .filter(|arg| arg.as_str() == "substituters")
                .count(),
            1
        );
        assert!(!args.iter().any(|arg| arg.contains("trusted-public-keys")));
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
            (
                ("root.drv".to_owned(), "out".to_owned()),
                "/nix/store/root".to_owned(),
            ),
            (
                ("cached.drv".to_owned(), "out".to_owned()),
                "/nix/store/cached".to_owned(),
            ),
            (
                ("missing.drv".to_owned(), "out".to_owned()),
                "/nix/store/missing".to_owned(),
            ),
        ]);
        let availability = BTreeMap::from([
            ("/nix/store/root".to_owned(), ProbeResult::Missing),
            (
                "/nix/store/cached".to_owned(),
                ProbeResult::Present {
                    substituter: "https://cache.example".to_owned(),
                },
            ),
            ("/nix/store/missing".to_owned(), ProbeResult::Missing),
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
    fn parallel_probe_merges_a_hit_from_one_store_despite_another_missing() {
        // Both stores answer; the output is present in exactly one.
        let hits = BTreeMap::from([("/nix/store/a".to_owned(), true)]);
        let misses = BTreeMap::from([("/nix/store/a".to_owned(), false)]);
        let merged = merge_store_probes(&["store-1", "store-2"], &[hits, misses]);
        assert_eq!(
            merged["/nix/store/a"],
            ProbeResult::Present {
                substituter: "store-1".to_owned()
            }
        );
    }

    #[test]
    fn all_misses_yield_missing() {
        let merged = merge_store_probes(
            &["store-1", "store-2"],
            &[
                BTreeMap::from([("/nix/store/a".to_owned(), false)]),
                BTreeMap::from([("/nix/store/a".to_owned(), false)]),
            ],
        );
        assert_eq!(merged["/nix/store/a"], ProbeResult::Missing);
    }

    #[test]
    fn missing_plus_store_failure_is_indeterminate_not_missing() {
        let merged = merge_store_probes_with_errors(
            &["store-1", "store-2"],
            &[BTreeMap::from([("/nix/store/a".to_owned(), false)])],
            &["store-2"],
        );
        assert!(matches!(
            &merged["/nix/store/a"],
            ProbeResult::Indeterminate { errors } if errors.iter().all(|e| e == "store-2")
        ));
    }

    #[test]
    fn probe_merge_preserves_substituter_order_and_failure_attribution() {
        let output = "/nix/store/a".to_owned();
        let probes = vec![
            (
                "second".to_owned(),
                Ok(BTreeMap::from([(output.clone(), true)])),
            ),
            (
                "first".to_owned(),
                Ok(BTreeMap::from([(output.clone(), true)])),
            ),
        ];
        assert_eq!(
            merge_store_probe_results(std::slice::from_ref(&output), &probes, false)[&output],
            ProbeResult::Present {
                substituter: "second".to_owned()
            }
        );

        let failed = vec![(
            "private".to_owned(),
            Err(Error::PlannerCacheProbeFailed {
                substituter: "private".to_owned(),
                output: output.clone(),
                stderr: "down".to_owned(),
            }),
        )];
        assert_eq!(
            merge_store_probe_results(std::slice::from_ref(&output), &failed, true)[&output],
            ProbeResult::Indeterminate {
                errors: vec!["daemon".to_owned(), "private".to_owned()]
            }
        );
    }

    #[test]
    fn indeterminate_probe_always_fails_closed() {
        let route = select_route(
            "root.drv",
            &ProbeResult::Indeterminate {
                errors: vec!["cache".to_owned()],
            },
            &[],
            &BTreeMap::new(),
            "x86_64-linux",
            "x86_64-linux",
            "aarch64-linux",
            &RealizationPolicy::SubstituteOnly,
        );
        assert!(matches!(route, RealizationRoute::Fail { .. }));
    }

    #[test]
    fn dependency_order_and_plan_id_are_stable() {
        fn plan(drv: &str, dependencies: Vec<PlanDependency>) -> DerivationPlan {
            DerivationPlan {
                drv: drv.to_owned(),
                name: None,
                pname: None,
                output_names: vec![],
                outputs: vec![],
                system: "x86_64-linux".to_owned(),
                class: DerivationClass::BuildLocal,
                route: RealizationRoute::BuildLocal,
                hint: None,
                probe: None,
                actions: vec![],
                dependencies,
            }
        }
        let dependency = plan("/nix/store/z-dependency.drv", vec![]);
        let root = plan(
            "/nix/store/a-root.drv",
            vec![PlanDependency {
                drv: dependency.drv.clone(),
                outputs: vec!["out".to_owned()],
            }],
        );
        let ordered = dependency_first(
            vec![root.clone(), dependency.clone()],
            &[PlanRoot {
                drv: root.drv.clone(),
                outputs: vec!["out".to_owned()],
            }],
        );
        assert_eq!(ordered[0].drv, dependency.drv);
        assert_eq!(ordered[1].drv, root.drv);

        let plan = ClosurePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: "ignored".to_owned(),
            toplevel_installable: "/nix/store/source#top".to_owned(),
            flake_installable: "/nix/store/source#host".to_owned(),
            build_system: "x86_64-linux".to_owned(),
            host_system: "aarch64-linux".to_owned(),
            roots: vec![],
            derivations: ordered,
            counts: PlanCounts::default(),
        };
        assert_eq!(
            plan.canonical_plan_id().unwrap(),
            plan.canonical_plan_id().unwrap()
        );
    }

    #[test]
    fn installable_identity_is_part_of_plan_id() {
        let mut first = ClosurePlan {
            schema_version: PLAN_SCHEMA_VERSION,
            plan_id: String::new(),
            toplevel_installable: "/nix/store/source#top-a".to_owned(),
            flake_installable: "/nix/store/source#host".to_owned(),
            build_system: "x86_64-linux".to_owned(),
            host_system: "aarch64-linux".to_owned(),
            roots: vec![],
            derivations: vec![],
            counts: PlanCounts::default(),
        };
        let first_id = first.canonical_plan_id().unwrap();
        first.toplevel_installable = "/nix/store/source#top-b".to_owned();
        assert_ne!(first_id, first.canonical_plan_id().unwrap());
        first.toplevel_installable = "/nix/store/source#top-a".to_owned();
        first.flake_installable = "/nix/store/source#other-host".to_owned();
        assert_ne!(first_id, first.canonical_plan_id().unwrap());
    }

    #[test]
    fn builtin_dependency_is_ordered_before_its_consumer_and_build_local() {
        let builtin_route = select_route(
            "builtin.drv",
            &ProbeResult::SkippedKnownLocal,
            &[],
            &BTreeMap::new(),
            "builtin",
            "x86_64-linux",
            "aarch64-linux",
            &RealizationPolicy::SubstituteOnly,
        );
        assert_eq!(builtin_route, RealizationRoute::BuildLocal);

        let builtin = DerivationPlan {
            drv: "/nix/store/builtin.drv".to_owned(),
            name: None,
            pname: None,
            output_names: vec!["out".to_owned()],
            outputs: vec!["/nix/store/builtin".to_owned()],
            system: "builtin".to_owned(),
            class: DerivationClass::BuildLocal,
            route: builtin_route,
            hint: None,
            probe: Some(ProbeResult::SkippedKnownLocal),
            actions: vec![],
            dependencies: vec![],
        };
        let root = DerivationPlan {
            drv: "/nix/store/root.drv".to_owned(),
            name: None,
            pname: None,
            output_names: vec!["out".to_owned()],
            outputs: vec!["/nix/store/root".to_owned()],
            system: "x86_64-linux".to_owned(),
            class: DerivationClass::BuildLocal,
            route: RealizationRoute::BuildLocal,
            hint: None,
            probe: Some(ProbeResult::Missing),
            actions: vec![],
            dependencies: vec![PlanDependency {
                drv: builtin.drv.clone(),
                outputs: vec!["out".to_owned()],
            }],
        };
        let ordered = dependency_first(
            vec![root.clone(), builtin.clone()],
            &[PlanRoot {
                drv: root.drv,
                outputs: vec!["out".to_owned()],
            }],
        );
        assert_eq!(ordered[0].drv, builtin.drv);
    }

    #[test]
    fn local_presence_requires_a_daemon_probe_hit() {
        let path = "/nix/store/example".to_owned();
        let local = BTreeMap::from([(path.clone(), true)]);
        let availability = local
            .iter()
            .filter(|(_, present)| **present)
            .map(|(output, _)| (output.clone(), ProbeResult::LocalPresent))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(availability[&path], ProbeResult::LocalPresent);
    }

    #[test]
    fn known_local_hint_skips_probe_and_routes_build_local() {
        let hints = vec![RouteHintSpec {
            label: "identity-cli".into(),
            installable: ".#identity-cli".into(),
            on_miss: MissRoute::BuildLocal,
            origin: None,
            skip_probe: true,
        }];
        let hint_drvs = BTreeMap::from([("identity.drv".to_owned(), hints[0].clone())]);
        let output_paths = BTreeMap::from([(
            ("identity.drv".to_owned(), "out".to_owned()),
            "/nix/store/identity".to_owned(),
        )]);
        let skipped = hint_skip_probe_paths(&hint_drvs, &output_paths);
        assert_eq!(skipped, BTreeSet::from(["/nix/store/identity".to_owned()]));

        let probe = derive_probe_result(
            "identity.drv",
            &hint_drvs,
            &["/nix/store/identity".to_owned()],
            &BTreeMap::new(),
        );
        assert_eq!(probe, ProbeResult::SkippedKnownLocal);

        let route = select_route(
            "identity.drv",
            &probe,
            &hints,
            &hint_drvs,
            "x86_64-linux",
            "x86_64-linux",
            "aarch64-linux",
            &RealizationPolicy::SubstituteOnly,
        );
        assert_eq!(route, RealizationRoute::BuildLocal);
    }

    #[test]
    fn local_present_probe_routes_already_present() {
        let route = select_route(
            "local.drv",
            &ProbeResult::LocalPresent,
            &[],
            &BTreeMap::new(),
            "x86_64-linux",
            "x86_64-linux",
            "aarch64-linux",
            &RealizationPolicy::SubstituteOnly,
        );
        assert_eq!(route, RealizationRoute::AlreadyPresent);
        assert_eq!(route_class(&route), DerivationClass::LocalPresent);
    }

    #[test]
    fn hint_on_miss_fail_blocks_substitution_requirement() {
        let spec = RouteHintSpec {
            label: "must-substitute".into(),
            installable: ".#pkg".into(),
            on_miss: MissRoute::Fail,
            origin: None,
            skip_probe: false,
        };
        let route = route_for_hint(
            &spec,
            &[],
            "aarch64-linux",
            "x86_64-linux",
            &RealizationPolicy::SubstituteOnly,
        );
        assert!(matches!(route, RealizationRoute::Fail { .. }));
    }

    #[test]
    fn remote_native_route_requires_a_declared_builder() {
        let spec = RouteHintSpec {
            label: "remote".into(),
            installable: ".#pkg".into(),
            on_miss: MissRoute::RemoteNative,
            origin: None,
            skip_probe: false,
        };
        let no_builder = route_for_hint(
            &spec,
            &[],
            "aarch64-linux",
            "x86_64-linux",
            &RealizationPolicy::SubstituteOnly,
        );
        assert!(matches!(no_builder, RealizationRoute::Fail { .. }));

        let with_builder = route_for_hint(
            &spec,
            &[],
            "aarch64-linux",
            "x86_64-linux",
            &RealizationPolicy::RemoteNative {
                builders: vec!["ssh://builder".into()],
            },
        );
        assert_eq!(
            with_builder,
            RealizationRoute::RemoteNative {
                builders: vec!["ssh://builder".into()]
            }
        );
    }

    #[test]
    fn build_local_hint_on_wrong_system_fails() {
        let spec = RouteHintSpec {
            label: "cross".into(),
            installable: ".#pkg".into(),
            on_miss: MissRoute::BuildLocal,
            origin: None,
            skip_probe: false,
        };
        let route = route_for_hint(
            &spec,
            &[],
            "aarch64-linux",
            "x86_64-linux",
            &RealizationPolicy::SubstituteOnly,
        );
        assert!(matches!(route, RealizationRoute::Fail { .. }));
    }

    #[test]
    fn display_label_prefers_package_name_and_keeps_derivation_name() {
        let plan = DerivationPlan {
            drv: "/nix/store/hash-canix-1.0.drv".to_owned(),
            name: Some("canix-1.0".to_owned()),
            pname: Some("canix".to_owned()),
            output_names: Vec::new(),
            outputs: Vec::new(),
            system: "x86_64-linux".to_owned(),
            class: DerivationClass::BuildLocal,
            route: RealizationRoute::BuildLocal,
            hint: None,
            probe: None,
            actions: Vec::new(),
            dependencies: Vec::new(),
        };

        assert_eq!(plan.display_label(), "canix (canix-1.0)");
    }
}
