//! Generic prepared-state persistence for crossbow prerequisite roots.
//!
//! The state file is a JSON document keyed by frozen flake ref.  Roots are
//! stored as per-label maps so callers can detect which specific labels
//! changed between prepare runs without re-evaluating Nix.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{LabeledRoot, Result};

/// The durable state persisted after a successful `crossbow prepare`.
///
/// # Format
///
/// ```json
/// {
///   "frozen_flake_ref": "/nix/store/...-source#host-crossbow",
///   "roots": {"system-toplevel": "/nix/store/aaa...", "identity-cli": "/nix/store/bbb..."},
///   "fingerprints": {"system-toplevel": "sha256-...", "identity-cli": "sha256-..."},
///   "prepared_at": "2026-06-22T08:00:00+02:00"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedState {
    /// Frozen flake reference at the time the roots were published.
    pub frozen_flake_ref: String,
    /// Label → store path mapping for each prerequisite root.
    pub roots: BTreeMap<String, String>,
    /// Label → per-root sha256 fingerprint.
    #[serde(default)]
    pub fingerprints: BTreeMap<String, String>,
    /// ISO 8601 timestamp of when the state was saved.
    #[serde(default)]
    pub prepared_at: Option<String>,
}

/// Why a saved prepared state is not usable for the current rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateStatus {
    /// No saved state file exists.
    Missing,
    /// The flake source changed since last prepare.
    Stale {
        /// Frozen flake ref at the time of the last prepare.
        old_flake_ref: String,
        /// Current frozen flake ref.
        new_flake_ref: String,
    },
    /// All checks passed — the saved roots are still valid.
    Current(PreparedState),
}

/// Persist a prepared state to `path`, creating parent directories as needed.
pub fn save_prepared_state(path: &Path, state: &PreparedState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| crate::Error::NixStoreRead {
            file: parent.display().to_string(),
            source: e,
        })?;
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|source| crate::Error::MetadataJson { source })?;
    fs::write(path, json).map_err(|e| crate::Error::NixStoreRead {
        file: path.display().to_string(),
        source: e,
    })?;
    Ok(())
}

/// Load a prepared state from `path`, keyed by `frozen_flake_ref`.
///
/// Returns:
/// - `StateStatus::Missing` when the file does not exist.
/// - `StateStatus::Stale` when the file exists but the ref has changed
///   (the stale file is **deleted** before returning).
/// - `StateStatus::Current(prepared_state)` when the ref matches.
pub fn load_prepared_state(path: &Path, frozen_flake_ref: &str) -> Result<StateStatus> {
    let json = match fs::read_to_string(path) {
        Ok(json) => json,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StateStatus::Missing);
        }
        Err(e) => {
            return Err(crate::Error::NixStoreRead {
                file: path.display().to_string(),
                source: e,
            });
        }
    };

    let state: PreparedState =
        serde_json::from_str(&json).map_err(|source| crate::Error::MetadataJson { source })?;

    if state.frozen_flake_ref != frozen_flake_ref {
        let old_ref = state.frozen_flake_ref.clone();
        let _ = fs::remove_file(path);
        return Ok(StateStatus::Stale {
            old_flake_ref: old_ref,
            new_flake_ref: frozen_flake_ref.to_owned(),
        });
    }

    Ok(StateStatus::Current(state))
}

/// Path to the state file for a given host, rooted at `base_dir`.
pub fn state_file_path(base_dir: &Path, host: &str) -> PathBuf {
    base_dir.join(format!("crossbow-prepared-{host}.json"))
}

/// Convert a `PreparedState` into a `Vec<LabeledRoot>` for use with
/// `diff_roots`.
pub fn state_to_labeled_roots(state: &PreparedState) -> Vec<LabeledRoot> {
    state
        .roots
        .iter()
        .map(|(label, path)| LabeledRoot {
            label: label.clone(),
            path: path.clone(),
            fingerprint: state.fingerprints.get(label).cloned().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn simple_state() -> PreparedState {
        PreparedState {
            frozen_flake_ref: "/nix/store/abc#testhost-crossbow".into(),
            roots: BTreeMap::from([("system-toplevel".into(), "/nix/store/root1".into())]),
            fingerprints: BTreeMap::from([("system-toplevel".into(), "fp1".into())]),
            prepared_at: Some("2026-01-01T00:00:00Z".into()),
        }
    }

    #[test]
    fn save_and_load_round_trips() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let state = simple_state();

        save_prepared_state(&path, &state).unwrap();
        let loaded = load_prepared_state(&path, "/nix/store/abc#testhost-crossbow").unwrap();

        match loaded {
            StateStatus::Current(loaded_state) => {
                assert_eq!(loaded_state.frozen_flake_ref, state.frozen_flake_ref);
                assert_eq!(loaded_state.roots, state.roots);
                assert_eq!(loaded_state.fingerprints, state.fingerprints);
                assert_eq!(loaded_state.prepared_at, state.prepared_at);
            }
            other => panic!("expected Current, got {other:?}"),
        }
    }

    #[test]
    fn load_returns_missing_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");

        let status = load_prepared_state(&path, "/nix/store/abc#host-crossbow").unwrap();
        assert_eq!(status, StateStatus::Missing);
    }

    #[test]
    fn load_returns_stale_when_ref_differs_and_deletes_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        save_prepared_state(&path, &simple_state()).unwrap();

        let status = load_prepared_state(&path, "/nix/store/new#host-crossbow").unwrap();

        match status {
            StateStatus::Stale {
                old_flake_ref,
                new_flake_ref,
            } => {
                assert_eq!(old_flake_ref, "/nix/store/abc#testhost-crossbow");
                assert_eq!(new_flake_ref, "/nix/store/new#host-crossbow");
            }
            other => panic!("expected Stale, got {other:?}"),
        }

        // State file should be deleted.
        assert!(!path.exists(), "stale state file should be deleted");
    }

    #[test]
    fn state_to_labeled_roots_converts_correctly() {
        let state = PreparedState {
            frozen_flake_ref: "ref".into(),
            roots: BTreeMap::from([
                ("system-toplevel".into(), "/nix/store/path1".into()),
                ("identity-cli".into(), "/nix/store/path2".into()),
            ]),
            fingerprints: BTreeMap::from([("system-toplevel".into(), "fp1".into())]),
            prepared_at: None,
        };

        let labeled = state_to_labeled_roots(&state);
        assert_eq!(labeled.len(), 2);
        assert_eq!(labeled[0].label, "identity-cli");
        assert_eq!(labeled[0].path, "/nix/store/path2");
        assert_eq!(labeled[0].fingerprint, "");
        assert_eq!(labeled[1].label, "system-toplevel");
        assert_eq!(labeled[1].path, "/nix/store/path1");
        assert_eq!(labeled[1].fingerprint, "fp1");
    }

    #[test]
    fn load_tolerates_legacy_format_without_fingerprints() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("state.json");
        let legacy = serde_json::json!({
            "frozen_flake_ref": "/nix/store/abc#host-crossbow",
            "roots": { "root-0": "/nix/store/root1" }
        });
        fs::write(&path, legacy.to_string()).unwrap();

        let status = load_prepared_state(&path, "/nix/store/abc#host-crossbow").unwrap();
        match status {
            StateStatus::Current(s) => {
                assert_eq!(
                    s.roots.get("root-0").map(String::as_str),
                    Some("/nix/store/root1")
                );
                assert!(s.fingerprints.is_empty());
                assert!(s.prepared_at.is_none());
            }
            other => panic!("expected Current, got {other:?}"),
        }
    }

    #[test]
    fn state_file_path_joins_host_name() {
        let base = Path::new("/tmp/canix");
        assert_eq!(
            state_file_path(base, "thething"),
            PathBuf::from("/tmp/canix/crossbow-prepared-thething.json")
        );
    }
}
