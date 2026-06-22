//! Parsed contents of a Crossbow requirements artifact (the output of
//! `mkRequirementsArtifact` in `lib/nixos-systems.nix`).

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
}

impl RequirementsArtifact {
    /// Read a requirements artifact from `artifact_path`, which is the
    /// output path of a realised `crossbowRequirements` derivation.
    pub fn read(artifact_path: &Path) -> Result<Self> {
        let roots = read_lines_if_exists(artifact_path.join("roots"), "roots")?;
        let drvs = read_lines_if_exists(artifact_path.join("drvs"), "drvs")?;
        let fingerprint = match read_file_optional(&artifact_path.join("fingerprint")) {
            Ok(Some(raw)) => Some(raw.trim().to_owned()),
            Ok(None) => None,
            Err(e) => return Err(e),
        };
        Ok(Self {
            roots,
            drvs,
            fingerprint,
        })
    }
}

fn read_file_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::NixStoreRead {
            file: path.display().to_string(),
            source: e,
        }),
    }
}

fn read_lines_if_exists(path: impl AsRef<Path>, label: &str) -> Result<Vec<String>> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).map_err(|e| Error::NixStoreRead {
        file: path.display().to_string(),
        source: e,
    })?;
    Ok(parse_requirement_lines(&raw, label))
}

fn parse_requirement_lines(raw: &str, _label: &str) -> Vec<String> {
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

    #[test]
    fn reads_roots_drvs_and_fingerprint() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "/nix/store/aaa-toplevel\n/nix/store/bbb-etc\n");
        write_file(
            &dir,
            "drvs",
            "/nix/store/aaa.drv\n/nix/store/bbb.drv\n",
        );
        write_file(
            &dir,
            "fingerprint",
            "sha256-abc123\n",
        );

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
        assert_eq!(artifact.drvs, vec!["/nix/store/aaa.drv"]);
        assert!(artifact.fingerprint.is_none());
    }

    #[test]
    fn skips_blank_lines_and_whitespace() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "\n/nix/store/one\n  \n/nix/store/two  \n");
        write_file(&dir, "drvs", "/nix/store/one.drv\n");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert_eq!(
            artifact.roots,
            vec!["/nix/store/one", "/nix/store/two"]
        );
    }

    #[test]
    fn empty_artifact_has_empty_roots_and_drvs() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "roots", "");
        write_file(&dir, "drvs", "");

        let artifact = RequirementsArtifact::read(dir.path()).unwrap();
        assert!(artifact.roots.is_empty());
        assert!(artifact.drvs.is_empty());
        assert!(artifact.fingerprint.is_none());
    }
}
