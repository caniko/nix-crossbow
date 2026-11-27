#![cfg(unix)]

use std::{cell::RefCell, fs, os::unix::fs::PermissionsExt, path::Path};

use crossbow_switch::{
    Error, PreparedSwitch, Publisher, Result,
    audit::{
        AuditError, AuditFindingKind, AuditOptions, AuditingPublisher, audit_paths,
        audit_prepared_switch,
    },
};

#[test]
fn rejects_prepared_switch_without_a_captured_closure() {
    let prepared = PreparedSwitch {
        frozen_flake: "path:/fixture".into(),
        toplevel_attr: "fixture".into(),
        toplevel: "/nix/store/fixture".into(),
        closure: vec![],
        fingerprint: "uncaptured".into(),
    };
    assert!(matches!(
        audit_prepared_switch(&prepared, &AuditOptions::new("x86_64-linux")),
        Err(AuditError::UncapturedClosure)
    ));
}

#[derive(Default)]
struct Cache(RefCell<Vec<Vec<String>>>);

impl Publisher for Cache {
    fn publish(&self, paths: &[String]) -> Result<()> {
        self.0.borrow_mut().push(paths.to_vec());
        Ok(())
    }
}

fn executable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn scans_elf_executables_and_shebangs_before_publication() {
    let dir = tempfile::tempdir().unwrap();
    let interpreter = dir.path().join("shell");
    let script = dir.path().join("script");
    // A big-endian header exercises EI_DATA as well as e_machine.
    let mut elf = [0; 64];
    elf[..6].copy_from_slice(b"\x7fELF\x02\x02");
    elf[18..20].copy_from_slice(&0xb7_u16.to_be_bytes());
    executable(&interpreter, &elf);
    executable(
        &script,
        format!("#!{} -e\n", interpreter.display()).as_bytes(),
    );
    let paths = vec![dir.path().display().to_string()];

    let mut options = AuditOptions::new("aarch64-linux");
    options.store_dir = dir.path();
    let report = audit_paths(&paths, &options).unwrap();
    assert_eq!(report.elf_executables, 1);
    assert_eq!(report.scripts, 1);

    let cache = Cache::default();
    let mut publisher = AuditingPublisher {
        delegate: &cache,
        options,
    };
    publisher.options.target_system = "x86_64-linux";
    let Err(Error::ClosureAudit(AuditError::Findings(findings))) = publisher.publish(&paths) else {
        panic!("expected a structured architecture failure");
    };
    assert_eq!(findings.len(), 2);
    assert!(findings.iter().all(|f| matches!(
        f.kind,
        AuditFindingKind::WrongMachine {
            actual: 0xb7,
            expected: 0x3e
        }
    )));
    assert!(
        findings
            .iter()
            .any(|f| f.path == interpreter && f.interpreter.is_none())
    );
    assert!(
        findings
            .iter()
            .any(|f| f.path == script && f.interpreter.as_ref() == Some(&interpreter))
    );
    assert!(cache.0.borrow().is_empty());

    publisher.options.target_system = "aarch64-linux";
    publisher.publish(&paths).unwrap();
    assert_eq!(*cache.0.borrow(), [paths]);
}

#[test]
fn allowlist_can_exclude_a_foreign_binary_but_not_its_use_as_an_interpreter() {
    let dir = tempfile::tempdir().unwrap();
    let foreign = dir.path().join("foreign");
    let mut elf = [0; 64];
    elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
    elf[18..20].copy_from_slice(&0xb7_u16.to_le_bytes());
    executable(&foreign, &elf);
    let paths = vec![dir.path().display().to_string()];
    let exceptions = [foreign.clone()];
    let mut options = AuditOptions::new("x86_64-linux");
    options.store_dir = dir.path();
    options.allowlist = &exceptions;
    assert_eq!(audit_paths(&paths, &options).unwrap().skipped_entries, 1);

    let script = dir.path().join("script");
    executable(&script, format!("#!{}\n", foreign.display()).as_bytes());
    let Err(AuditError::Findings(findings)) = audit_paths(&paths, &options) else {
        panic!("expected the audited script to reject its foreign interpreter");
    };
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].path, script);
}

#[test]
fn rejects_interpreter_cycles_and_missing_interpreters() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("script");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&script, &alias).unwrap();
    executable(&script, format!("#!{}\n", alias.display()).as_bytes());
    let paths = vec![script.display().to_string()];
    let mut options = AuditOptions::new("x86_64-linux");
    options.store_dir = dir.path();
    let Err(AuditError::Findings(findings)) = audit_paths(&paths, &options) else {
        panic!("expected a cycle finding");
    };
    assert_eq!(findings[0].kind, AuditFindingKind::InterpreterLoop);
    fs::remove_file(&alias).unwrap();
    assert!(matches!(
        audit_paths(&paths, &options),
        Err(AuditError::Io { path, source, .. })
            if path == alias && source.kind() == std::io::ErrorKind::NotFound
    ));
}
