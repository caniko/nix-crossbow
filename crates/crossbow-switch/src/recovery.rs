//! Native recovery is explicit and separate from the planned switch executor.
//! The caller owns the choice of runner, cache, and prepared-state storage.

use std::collections::BTreeSet;
use std::path::Path;

use crate::{
    Error, LabeledRoot, Publisher, RequirementsArtifact, Result, RootDiff, Verifier,
    closure_to_publish, diff_roots, label_roots, require_cache_paths,
};

/// Store and state operations for native prerequisite recovery.
pub trait RecoveryRunner {
    /// Realize a batch of distinct derivations and make the requested outputs
    /// available to the closure query and publisher (copying them if needed).
    fn realize(&self, drvs: &[String], roots: &[String]) -> Result<()>;

    /// Return the paths to publish for a realized root. Defaults to a local
    /// Nix store query; callers may supply their own closure policy.
    fn closure(&self, root: &str) -> Result<Vec<String>> {
        closure_to_publish(Path::new(root))
    }

    /// Record the verified set before cleanup. Return an error if the state
    /// transition failed and the old roots must remain protected.
    fn record_prepared(&self, roots: &[LabeledRoot]) -> Result<()>;

    /// Release this caller's retention for a stale path. Do not release
    /// another caller's pin; this operation should be safe to retry.
    fn remove_stale_root(&self, path: &str) -> Result<()>;
}

/// Result of verified and recorded native recovery.
#[derive(Debug)]
#[non_exhaustive]
pub struct RecoveryReport {
    /// Current roots in artifact order.
    pub roots: Vec<LabeledRoot>,
    /// Label-level changes relative to the previous prepared state.
    pub diff: RootDiff,
    /// Deduplicated cache paths published, including realized roots.
    pub published_paths: Vec<String>,
    /// Stale paths successfully released.
    pub removed_stale_roots: Vec<String>,
    /// Stale paths to retry and why they could not be released. These do not
    /// invalidate the verified and recorded state.
    pub cleanup_failures: Vec<(String, Error)>,
}

/// Prepare the added or changed roots and recover unchanged roots missing
/// from the cache. Publish one deduplicated closure batch, verify the published
/// paths and all current roots, and record success before releasing stale
/// paths. Only paths absent from the new root set are eligible for cleanup.
///
/// The artifact's roots and derivations must be paired by index. Callers must
/// serialize operations that share the same prepared state and retention.
pub fn prepare_native_recovery(
    requirements: &RequirementsArtifact,
    previous: &[LabeledRoot],
    runner: &dyn RecoveryRunner,
    publisher: &dyn Publisher,
    verifier: &dyn Verifier,
) -> Result<RecoveryReport> {
    if requirements.roots.len() != requirements.drvs.len() {
        return Err(Error::InvalidRecoveryRequirements(
            "expected one derivation per root".into(),
        ));
    }
    let roots = label_roots(requirements);
    for set in [previous, &roots] {
        let mut labels = BTreeSet::new();
        for root in set {
            if !labels.insert(&root.label) {
                return Err(Error::InvalidRecoveryRequirements(format!(
                    "duplicate root label {:?}",
                    root.label
                )));
            }
        }
    }
    for path in requirements
        .roots
        .iter()
        .chain(&requirements.drvs)
        .chain(previous.iter().map(|root| &root.path))
    {
        if !Path::new(path).is_absolute() {
            return Err(Error::InvalidRecoveryRequirements(format!(
                "expected an absolute path, got {path:?}"
            )));
        }
    }

    let diff = diff_roots(previous, &roots);
    let unchanged = unique_paths(&diff.unchanged);
    let missing = if unchanged.is_empty() {
        Vec::new()
    } else {
        verifier.verify_present(&Vec::from_iter(unchanged.iter().cloned()))?
    };
    let changed_labels = diff
        .added
        .iter()
        .chain(&diff.changed)
        .map(|root| root.label.as_str())
        .collect::<BTreeSet<_>>();
    let missing = missing.into_iter().collect::<BTreeSet<_>>();
    let mut outputs = BTreeSet::new();
    let mut drvs = BTreeSet::new();
    for (root, drv) in roots.iter().zip(&requirements.drvs) {
        if changed_labels.contains(root.label.as_str()) || missing.contains(&root.path) {
            outputs.insert(root.path.clone());
            drvs.insert(drv.clone());
        }
    }
    let mut publish = BTreeSet::new();
    if !outputs.is_empty() {
        let outputs = Vec::from_iter(outputs);
        runner.realize(&Vec::from_iter(drvs), &outputs)?;
        for root in outputs {
            publish.extend(runner.closure(&root)?);
            publish.insert(root);
        }
    }
    let current = unique_paths(&roots);
    let verify = publish
        .iter()
        .chain(&current)
        .cloned()
        .collect::<BTreeSet<_>>();
    let published_paths: Vec<_> = publish.into_iter().collect();
    if !published_paths.is_empty() {
        publisher.publish(&published_paths)?;
    }
    if !verify.is_empty() {
        require_cache_paths(verifier, &Vec::from_iter(verify))?;
    }
    runner.record_prepared(&roots)?;

    let mut report = RecoveryReport {
        roots,
        diff,
        published_paths,
        removed_stale_roots: Vec::new(),
        cleanup_failures: Vec::new(),
    };
    for path in unique_paths(previous).difference(&current) {
        match runner.remove_stale_root(path) {
            Ok(()) => report.removed_stale_roots.push(path.clone()),
            Err(error) => report.cleanup_failures.push((path.clone(), error)),
        }
    }
    Ok(report)
}

fn unique_paths(roots: &[LabeledRoot]) -> BTreeSet<String> {
    roots.iter().map(|root| root.path.clone()).collect()
}
