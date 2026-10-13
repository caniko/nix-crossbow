//! Parsed contents of a Crossbow requirements artifact (the output of
//! `mkRequirementsArtifact` in `lib/nixos-systems.nix`).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{Error, Result};

/// Parsed contents of a `crossbowRequirements` derivation output directory.
///
/// The artifact is produced by `mkRequirementsArtifact` in Crossbow's Nix
/// library.  It contains a `roots` file (one realised store path per line),
/// a `drvs` file (one `.drv` path per line), and optional metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementsArtifact {
    /// Realised store paths that must be pre-published before activation.
    pub roots: Vec<String>,
    /// `.drv` paths whose realisation produces the corresponding `roots`.
    pub drvs: Vec<String>,
    /// Stable sha256 fingerprint of the roots list.
    ///
    /// `None` for artifacts produced by older Crossbow versions that did not
    /// emit a fingerprint.  Equal fingerprints imply equal root sets.
    pub fingerprint: Option<String>,
    /// Human-readable labels for each root, in the same order as `roots`.
    ///
    /// Empty when the artifact was produced by an older Crossbow version that
    /// did not emit per-root labels.
    pub root_labels: Vec<String>,
    /// Per-root sha256 fingerprints, in the same order as `roots`.
    ///
    /// Empty when the artifact was produced by an older Crossbow version.
    pub root_fingerprints: Vec<String>,
}

impl RequirementsArtifact {
    /// Read a requirements artifact from `artifact_path`, which is the
    /// output path of a realised `crossbowRequirements` derivation.
    pub fn read(artifact_path: &Path) -> Result<Self> {
        let roots = read_lines(&artifact_path.join("roots"))?;
        let drvs = read_lines(&artifact_path.join("drvs"))?;
        let fingerprint = read_file_optional(&artifact_path.join("fingerprint"))?;
        let root_labels = read_lines_optional(&artifact_path.join("root-labels"))?;
        let root_fingerprints = read_lines_optional(&artifact_path.join("root-fingerprints"))?;
        Ok(Self {
            roots,
            drvs,
            fingerprint,
            root_labels,
            root_fingerprints,
        })
    }
}

/// One labeled prerequisite root, assembled from a requirements artifact.
///
/// A labeled root pairs a human-readable label (e.g. `"system-toplevel"`,
/// `"identity-cli"`) with its store path and per-root fingerprint.  Used to
/// track which roots changed between prepare runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabeledRoot {
    /// Human-readable label such as `"system-toplevel"` or `"identity-cli"`.
    pub label: String,
    /// Realised store path of this root.
    pub path: String,
    /// Per-root sha256 fingerprint.  Equal fingerprints imply an unchanged root.
    pub fingerprint: String,
}

/// Pair up `roots`, `root_labels`, and `root_fingerprints` from an artifact.
///
/// When the artifact has no labels or fingerprints (legacy format), synthetic
/// labels `root-0`, `root-1`, … are generated and fingerprints are left empty.
/// This ensures the caller can always iterate over labeled roots.
pub fn label_roots(artifact: &RequirementsArtifact) -> Vec<LabeledRoot> {
    let labels: Vec<String> = if artifact.root_labels.len() == artifact.roots.len() {
        artifact.root_labels.clone()
    } else {
        (0..artifact.roots.len())
            .map(|i| format!("root-{i}"))
            .collect()
    };
    let fingerprints: Vec<String> = if artifact.root_fingerprints.len() == artifact.roots.len() {
        artifact.root_fingerprints.clone()
    } else {
        vec![String::new(); artifact.roots.len()]
    };
    artifact
        .roots
        .iter()
        .enumerate()
        .map(|(i, path)| LabeledRoot {
            label: labels[i].clone(),
            path: path.clone(),
            fingerprint: fingerprints[i].clone(),
        })
        .collect()
}

/// What changed between two sets of labeled roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootDiff {
    /// Labels present in `new` but absent from `old`.
    pub added: Vec<LabeledRoot>,
    /// Labels present in both but with a different fingerprint.
    pub changed: Vec<LabeledRoot>,
    /// Labels present in both with the same fingerprint.
    pub unchanged: Vec<LabeledRoot>,
    /// Labels present in `old` but absent from `new`.
    pub removed: Vec<String>,
}

/// Compare old and new labeled roots, returning which labels are new, changed,
/// unchanged, or removed.
pub fn diff_roots(old: &[LabeledRoot], new: &[LabeledRoot]) -> RootDiff {
    let old_by_label: BTreeMap<&str, &LabeledRoot> =
        old.iter().map(|r| (r.label.as_str(), r)).collect();
    let new_by_label: BTreeMap<&str, &LabeledRoot> =
        new.iter().map(|r| (r.label.as_str(), r)).collect();

    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged = Vec::new();

    for new_root in new {
        match old_by_label.get(new_root.label.as_str()) {
            None => {
                added.push(new_root.clone());
            }
            Some(old_root) => {
                if old_root.fingerprint == new_root.fingerprint && !new_root.fingerprint.is_empty()
                {
                    unchanged.push(new_root.clone());
                } else {
                    changed.push(new_root.clone());
                }
            }
        }
    }

    let removed: Vec<String> = old
        .iter()
        .map(|r| r.label.clone())
        .filter(|label| !new_by_label.contains_key(label.as_str()))
        .collect();

    RootDiff {
        added,
        changed,
        unchanged,
        removed,
    }
}

fn read_file_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::NixStoreRead {
            file: path.display().to_string(),
            source: e,
        }),
    }
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    let raw = fs::read_to_string(path).map_err(|e| Error::NixStoreRead {
        file: path.display().to_string(),
        source: e,
    })?;
    Ok(parse_lines(&raw))
}

fn read_lines_optional(path: &Path) -> Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(parse_lines(&raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::NixStoreRead {
            file: path.display().to_string(),
            source: e,
        }),
    }
}

fn parse_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &TempDir, name: &str, content: &str) {
        fs::write(dir.path().join(name), content).unwrap();
    }

    // --- artifact parsing tests ---

    #[test]
    fn reads_roots_drvs_and_fingerprint() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "roots",
            "/nix/store/aaa-toplevel\n/nix/store/bbb-etc\n",
        );
        write_file(&dir, "drvs", "/nix/store/aaa.drv\n/nix/store/bbb.drv\n");
        write_file(&dir, "fingerprint", "sha256-abc123\n");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert_eq!(
            artifact.roots,
            vec!["/nix/store/aaa-toplevel", "/nix/store/bbb-etc"]
        );
        assert_eq!(
            artifact.drvs,
            vec!["/nix/store/aaa.drv", "/nix/store/bbb.drv"]
        );
        assert_eq!(artifact.fingerprint.as_deref(), Some("sha256-abc123"));
    }

    #[test]
    fn tolerates_missing_fingerprint() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "/nix/store/aaa\n");
        write_file(&dir, "drvs", "/nix/store/aaa.drv\n");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert_eq!(artifact.roots, vec!["/nix/store/aaa"]);
        assert!(artifact.fingerprint.is_none());
    }

    #[test]
    fn skips_blank_lines_and_whitespace() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "\n/nix/store/one\n  \n/nix/store/two  \n");
        write_file(&dir, "drvs", "/nix/store/one.drv\n");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert_eq!(artifact.roots, vec!["/nix/store/one", "/nix/store/two"]);
    }

    #[test]
    fn empty_artifact_has_empty_roots_and_drvs() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "");
        write_file(&dir, "drvs", "");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert!(artifact.roots.is_empty());
        assert!(artifact.drvs.is_empty());
    }

    #[test]
    fn reads_root_labels_and_fingerprints() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "/nix/store/aaa\n/nix/store/bbb\n");
        write_file(&dir, "drvs", "/nix/store/aaa.drv\n/nix/store/bbb.drv\n");
        write_file(&dir, "root-labels", "system-toplevel\nidentity-cli\n");
        write_file(&dir, "root-fingerprints", "sha256-one\nsha256-two\n");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert_eq!(
            artifact.root_labels,
            vec!["system-toplevel", "identity-cli"]
        );
        assert_eq!(artifact.root_fingerprints, vec!["sha256-one", "sha256-two"]);
    }

    #[test]
    fn tolerates_missing_labels_and_fingerprints() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "/nix/store/aaa\n");
        write_file(&dir, "drvs", "/nix/store/aaa.drv\n");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert!(artifact.root_labels.is_empty());
        assert!(artifact.root_fingerprints.is_empty());
    }

    // --- label_roots tests ---

    #[test]
    fn label_roots_pairs_labels_when_available() {
        let artifact = RequirementsArtifact {
            roots: vec!["/nix/store/aaa".into(), "/nix/store/bbb".into()],
            drvs: vec![],
            fingerprint: None,
            root_labels: vec!["system-toplevel".into(), "identity-cli".into()],
            root_fingerprints: vec!["fp1".into(), "fp2".into()],
        };
        let labeled = label_roots(&artifact);
        assert_eq!(labeled.len(), 2);
        assert_eq!(labeled[0].label, "system-toplevel");
        assert_eq!(labeled[0].path, "/nix/store/aaa");
        assert_eq!(labeled[0].fingerprint, "fp1");
        assert_eq!(labeled[1].label, "identity-cli");
        assert_eq!(labeled[1].path, "/nix/store/bbb");
        assert_eq!(labeled[1].fingerprint, "fp2");
    }

    #[test]
    fn label_roots_generates_synthetic_labels_when_missing() {
        let artifact = RequirementsArtifact {
            roots: vec!["/nix/store/aaa".into(), "/nix/store/bbb".into()],
            drvs: vec![],
            fingerprint: None,
            root_labels: vec![],
            root_fingerprints: vec![],
        };
        let labeled = label_roots(&artifact);
        assert_eq!(labeled.len(), 2);
        assert_eq!(labeled[0].label, "root-0");
        assert_eq!(labeled[1].label, "root-1");
        assert!(labeled[0].fingerprint.is_empty());
        assert!(labeled[1].fingerprint.is_empty());
    }

    // --- diff_roots tests ---

    fn root(label: &str, path: &str, fp: &str) -> LabeledRoot {
        LabeledRoot {
            label: label.into(),
            path: path.into(),
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn diff_reports_added_when_new_label_appears() {
        let old = vec![];
        let new = vec![root("system-toplevel", "/nix/store/aaa", "fp1")];

        let diff = diff_roots(&old, &new);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].label, "system-toplevel");
        assert!(diff.changed.is_empty());
        assert!(diff.unchanged.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_reports_changed_when_fingerprint_differs() {
        let old = vec![root("system-toplevel", "/nix/store/old", "fp-old")];
        let new = vec![root("system-toplevel", "/nix/store/new", "fp-new")];

        let diff = diff_roots(&old, &new);
        assert!(diff.added.is_empty());
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].label, "system-toplevel");
        assert!(diff.unchanged.is_empty());
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_reports_unchanged_when_fingerprint_matches() {
        let old = vec![root("system-toplevel", "/nix/store/aaa", "fp1")];
        let new = vec![root("system-toplevel", "/nix/store/aaa", "fp1")];

        let diff = diff_roots(&old, &new);
        assert!(diff.added.is_empty());
        assert!(diff.changed.is_empty());
        assert_eq!(diff.unchanged.len(), 1);
        assert!(diff.removed.is_empty());
    }

    #[test]
    fn diff_reports_removed_when_label_disappears() {
        let old = vec![root("system-toplevel", "/nix/store/aaa", "fp1")];
        let new = vec![];

        let diff = diff_roots(&old, &new);
        assert!(diff.added.is_empty());
        assert!(diff.changed.is_empty());
        assert!(diff.unchanged.is_empty());
        assert_eq!(diff.removed, vec!["system-toplevel"]);
    }

    #[test]
    fn diff_handles_mixed_changes() {
        let old = vec![
            root("system-toplevel", "/nix/store/sys-old", "fp-sys-old"),
            root("identity-cli", "/nix/store/id-old", "fp-id"),
            root("removed-pkg", "/nix/store/rm", "fp-rm"),
        ];
        let new = vec![
            root("system-toplevel", "/nix/store/sys-new", "fp-sys-new"), // changed
            root("identity-cli", "/nix/store/id-old", "fp-id"),          // unchanged
            root("new-pkg", "/nix/store/new", "fp-new"),                 // added
        ];

        let diff = diff_roots(&old, &new);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].label, "new-pkg");
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].label, "system-toplevel");
        assert_eq!(diff.unchanged.len(), 1);
        assert_eq!(diff.unchanged[0].label, "identity-cli");
        assert_eq!(diff.removed, vec!["removed-pkg"]);
    }

    #[test]
    fn diff_treats_empty_fingerprint_as_changed() {
        let old = vec![root("pkg", "/nix/store/old", "")];
        let new = vec![root("pkg", "/nix/store/old", "")];

        let diff = diff_roots(&old, &new);
        // Empty fingerprints never match → always treated as changed.
        assert_eq!(diff.changed.len(), 1);
        assert!(diff.unchanged.is_empty());
    }
}
