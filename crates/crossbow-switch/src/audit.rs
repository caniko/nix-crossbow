//! Architecture checks for executable files in a realized Nix closure.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

use crate::{PreparedSwitch, Publisher};

/// Version of the scanner contract recorded in audit reports.
pub const SCANNER_VERSION: &str = "crossbow-closure-audit-v1";
const MAX_INTERPRETER_DEPTH: usize = 16;
const MAX_PREFIX_BYTES: u64 = 4096;

/// Caller-selected target and exceptions for an architecture audit.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct AuditOptions<'a> {
    /// Explicit Nix target system (for example `aarch64-linux`).
    pub target_system: &'a str,
    /// Only shebang interpreters under this absolute path are inspected.
    pub store_dir: &'a Path,
    /// Exact absolute paths of files or directory trees to skip, with no globbing.
    ///
    /// This does not exempt an interpreter used by a script outside the allowlist.
    pub allowlist: &'a [PathBuf],
}

impl<'a> AuditOptions<'a> {
    /// Audit for `target_system` using `/nix/store` and no exceptions.
    pub fn new(target_system: &'a str) -> Self {
        Self {
            target_system,
            store_dir: Path::new("/nix/store"),
            allowlist: &[],
        }
    }
}

/// Summary of a successful closure audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditReport {
    /// Scanner contract version.
    pub scanner_version: &'static str,
    /// Target system whose executables were checked.
    pub target_system: String,
    /// Exact candidate fingerprint, when auditing a prepared switch.
    pub prepared_fingerprint: Option<String>,
    /// Number of distinct canonical closure roots scanned.
    pub closure_paths: usize,
    /// Number of executable files with ELF headers checked.
    pub elf_executables: usize,
    /// Number of executable scripts with store shebangs checked.
    pub scripts: usize,
    /// Number of entries skipped by the allowlist (a pruned tree counts once).
    pub skipped_entries: usize,
}

/// A failure associated with an executable or its shebang interpreter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AuditFinding {
    /// Executable that triggered the finding.
    pub path: PathBuf,
    /// Interpreter where the failure occurred, if any.
    pub interpreter: Option<PathBuf>,
    /// Machine-readable reason for the finding.
    pub kind: AuditFindingKind,
}

/// Why a runtime entry did not pass the architecture audit.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum AuditFindingKind {
    /// ELF `e_machine` did not match the target.
    WrongMachine {
        /// Value read from the header.
        actual: u16,
        /// Value required by the target system.
        expected: u16,
    },
    /// ELF header is truncated or has an unsupported byte order.
    InvalidElfHeader,
    /// Script has an empty or unreadable interpreter token.
    InvalidShebang,
    /// Interpreter was not an executable ELF file or script.
    InvalidInterpreter,
    /// An in-store interpreter chain escaped the configured store.
    NonStoreInterpreter,
    /// Interpreter chain revisited a path or exceeded the scanner depth limit.
    InterpreterLoop,
}

/// A scanner error or a rejected closure.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum AuditError {
    /// Target has no supported ELF machine mapping.
    #[error("closure audit has no ELF machine mapping for {0}")]
    UnsupportedTarget(String),
    /// Store path or exception was not absolute.
    #[error("closure audit requires an absolute path: {}", .0.display())]
    RelativePath(PathBuf),
    /// An entry could not be resolved or read; the closure was not certified.
    #[error("closure audit could not {operation} {}: {source}", path.display())]
    Io {
        /// Operation that failed.
        operation: &'static str,
        /// Entry being inspected.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: io::Error,
    },
    /// The sorted list of audit findings.
    #[error("closure audit rejected {} entries; first finding: {:?}", .0.len(), .0.first())]
    Findings(Vec<AuditFinding>),
}

/// Audit the closure of an exact candidate and bind the report to its fingerprint.
pub fn audit_prepared_switch(
    prepared: &PreparedSwitch,
    options: &AuditOptions<'_>,
) -> Result<AuditReport, AuditError> {
    let mut report = audit_paths(&prepared.closure, options)?;
    report.prepared_fingerprint = Some(prepared.fingerprint.clone());
    Ok(report)
}

/// Gate publication on a successful audit. No paths are sent on failure.
pub struct AuditingPublisher<'a> {
    /// Downstream publisher.
    pub delegate: &'a dyn Publisher,
    /// Target and exceptions to apply to each publication.
    pub options: AuditOptions<'a>,
}

impl Publisher for AuditingPublisher<'_> {
    fn publish(&self, paths: &[String]) -> crate::Result<()> {
        audit_paths(paths, &self.options)?;
        self.delegate.publish(paths)
    }
}

/// Check executable ELF headers and store shebang chains in a complete closure.
///
/// Roots may be files or directories. Symlinks inside directories are skipped:
/// their Nix store targets are covered by the supplied roots, not by following
/// links into host directories. Absolute paths embedded in arbitrary file
/// contents and host shebangs (such as `/usr/bin/env`) are not audited.
pub fn audit_paths(
    paths: &[String],
    options: &AuditOptions<'_>,
) -> Result<AuditReport, AuditError> {
    let expected = match options.target_system {
        "x86_64-linux" => 0x003e,
        "aarch64-linux" => 0x00b7,
        "i686-linux" => 0x0003,
        "riscv64-linux" => 0x00f3,
        "armv7l-linux" => 0x0028,
        other => return Err(AuditError::UnsupportedTarget(other.into())),
    };
    for path in std::iter::once(options.store_dir)
        .chain(options.allowlist.iter().map(PathBuf::as_path))
        .chain(paths.iter().map(Path::new))
    {
        if !path.is_absolute() {
            return Err(AuditError::RelativePath(path.into()));
        }
    }

    let mut report = AuditReport {
        scanner_version: SCANNER_VERSION,
        target_system: options.target_system.into(),
        prepared_fingerprint: None,
        closure_paths: 0,
        elf_executables: 0,
        scripts: 0,
        skipped_entries: 0,
    };
    let allowed = |path: &Path| {
        options
            .allowlist
            .iter()
            .any(|entry| path.starts_with(entry))
    };
    let mut roots = BTreeSet::new();
    for path in paths.iter().collect::<BTreeSet<_>>() {
        if allowed(Path::new(path)) {
            report.skipped_entries += 1;
        } else {
            roots.insert(at(Path::new(path), "resolve", fs::canonicalize(path))?);
        }
    }
    report.closure_paths = roots.iter().filter(|path| !allowed(path)).count();
    let mut pending = Vec::from_iter(roots);
    let mut visited = BTreeSet::new();
    let mut findings = Vec::new();
    while let Some(path) = pending.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        if allowed(&path) {
            report.skipped_entries += 1;
            continue;
        }
        let meta = at(&path, "stat", fs::symlink_metadata(&path))?;
        if meta.is_dir() {
            for entry in at(&path, "list", fs::read_dir(&path))? {
                pending.push(at(&path, "list", entry)?.path());
            }
        } else if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
            let bytes = read_prefix(&path)?;
            let problem = if bytes.starts_with(b"\x7fELF") {
                report.elf_executables += 1;
                check_elf(&bytes, expected).map(|kind| (None, kind))
            } else if bytes.starts_with(b"#!") {
                match interpreter(&bytes) {
                    Some(next) if next.starts_with(options.store_dir) => {
                        report.scripts += 1;
                        check_interpreter(next, options.store_dir, expected)?
                            .map(|(path, kind)| (Some(path), kind))
                    }
                    Some(_) => None,
                    None => Some((None, AuditFindingKind::InvalidShebang)),
                }
            } else {
                None
            };
            if let Some((interpreter, kind)) = problem {
                findings.push(AuditFinding {
                    path,
                    interpreter,
                    kind,
                });
            }
        }
    }
    if findings.is_empty() {
        Ok(report)
    } else {
        findings.sort();
        Err(AuditError::Findings(findings))
    }
}

fn check_interpreter(
    first: &Path,
    store: &Path,
    expected: u16,
) -> Result<Option<(PathBuf, AuditFindingKind)>, AuditError> {
    use AuditFindingKind::*;

    let mut next = first.to_path_buf();
    let resolved_store = at(store, "resolve store", fs::canonicalize(store))?;
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_INTERPRETER_DEPTH {
        if !next.starts_with(store) && !next.starts_with(&resolved_store) {
            return Ok(Some((next, NonStoreInterpreter)));
        }
        let resolved = at(&next, "resolve interpreter", fs::canonicalize(&next))?;
        if !resolved.starts_with(&resolved_store) {
            return Ok(Some((resolved, NonStoreInterpreter)));
        }
        if !visited.insert(resolved.clone()) {
            return Ok(Some((resolved, InterpreterLoop)));
        }
        let meta = at(&resolved, "stat interpreter", fs::metadata(&resolved))?;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return Ok(Some((resolved, InvalidInterpreter)));
        }
        let bytes = read_prefix(&resolved)?;
        if bytes.starts_with(b"\x7fELF") {
            return Ok(check_elf(&bytes, expected).map(|kind| (resolved, kind)));
        }
        if !bytes.starts_with(b"#!") {
            return Ok(Some((resolved, InvalidInterpreter)));
        }
        let Some(path) = interpreter(&bytes) else {
            return Ok(Some((resolved, InvalidShebang)));
        };
        next = path.into();
    }
    Ok(Some((next, InterpreterLoop)))
}

fn interpreter(bytes: &[u8]) -> Option<&Path> {
    let line = bytes
        .strip_prefix(b"#!")?
        .split(|byte| *byte == b'\n')
        .next()?;
    let token = line
        .split(|byte| byte.is_ascii_whitespace())
        .find(|part| !part.is_empty())?;
    std::str::from_utf8(token).ok().map(Path::new)
}

fn check_elf(bytes: &[u8], expected: u16) -> Option<AuditFindingKind> {
    use AuditFindingKind::*;

    let Some(machine) = bytes.get(18..20).and_then(|pair| pair.try_into().ok()) else {
        return Some(InvalidElfHeader);
    };
    let actual = match bytes[5] {
        1 => u16::from_le_bytes(machine),
        2 => u16::from_be_bytes(machine),
        _ => return Some(InvalidElfHeader),
    };
    (actual != expected).then_some(WrongMachine { actual, expected })
}

fn read_prefix(path: &Path) -> Result<Vec<u8>, AuditError> {
    let mut file = at(path, "open", File::open(path))?.take(MAX_PREFIX_BYTES);
    let mut bytes = Vec::new();
    at(path, "read", file.read_to_end(&mut bytes))?;
    Ok(bytes)
}

fn at<T>(path: &Path, operation: &'static str, result: io::Result<T>) -> Result<T, AuditError> {
    result.map_err(|source| AuditError::Io {
        operation,
        path: path.into(),
        source,
    })
}
