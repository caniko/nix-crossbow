use std::cell::RefCell;

use crossbow_switch::{
    Error, LabeledRoot, Publisher, RequirementsArtifact, Result, Verifier,
    recovery::{RecoveryRunner, prepare_native_recovery},
};

#[derive(Default)]
struct Backend {
    events: RefCell<Vec<String>>,
    missing: Vec<String>,
    fail_receipt: bool,
}

impl Backend {
    fn log(&self, name: &str, values: &[String]) {
        self.events
            .borrow_mut()
            .push(format!("{name}:{}", values.join(",")));
    }
}

impl RecoveryRunner for Backend {
    fn realize(&self, drvs: &[String], roots: &[String]) -> Result<()> {
        self.log("realize", drvs);
        self.log("outputs", roots);
        Ok(())
    }

    fn closure(&self, root: &str) -> Result<Vec<String>> {
        self.log("closure", &[root.into()]);
        Ok(vec![root.into(), "/nix/store/dependency".into()])
    }

    fn record_prepared(&self, roots: &[LabeledRoot]) -> Result<()> {
        self.log(
            "record",
            &roots
                .iter()
                .map(|root| root.path.clone())
                .collect::<Vec<_>>(),
        );
        if self.fail_receipt {
            return Err(Error::CommandFailed {
                command: "record".into(),
                stderr: "disk full".into(),
            });
        }
        Ok(())
    }

    fn remove_stale_root(&self, path: &str) -> Result<()> {
        self.log("remove", &[path.into()]);
        Ok(())
    }
}

impl Publisher for Backend {
    fn publish(&self, paths: &[String]) -> Result<()> {
        self.log("publish", paths);
        Ok(())
    }
}

impl Verifier for Backend {
    fn verify_present(&self, paths: &[String]) -> Result<Vec<String>> {
        self.log("verify", paths);
        Ok(self
            .missing
            .iter()
            .filter(|path| paths.contains(path))
            .cloned()
            .collect())
    }
}

fn root(label: &str, path: &str, fingerprint: &str) -> LabeledRoot {
    LabeledRoot {
        label: label.into(),
        path: format!("/nix/store/{path}"),
        fingerprint: fingerprint.into(),
    }
}

fn artifact() -> RequirementsArtifact {
    RequirementsArtifact {
        roots: ["keep", "new", "add"]
            .map(|name| format!("/nix/store/{name}"))
            .into(),
        drvs: [
            "/nix/store/keep.drv",
            "/nix/store/shared.drv",
            "/nix/store/shared.drv",
        ]
        .map(String::from)
        .into(),
        root_labels: ["keep", "change", "add"].map(String::from).into(),
        root_fingerprints: ["keep", "new", "add"].map(String::from).into(),
        fingerprint: None,
    }
}

fn previous_roots() -> Vec<LabeledRoot> {
    vec![
        root("keep", "keep", "keep"),
        root("change", "old", "old"),
        root("removed-duplicate", "old", "old"),
        root("removed-but-shared", "keep", "keep"),
    ]
}

#[test]
fn batches_changed_and_added_roots_and_cleans_up_only_unreferenced_old_paths() {
    let backend = Backend::default();
    let report =
        prepare_native_recovery(&artifact(), &previous_roots(), &backend, &backend, &backend)
            .unwrap();
    assert_eq!(report.diff.changed, [root("change", "new", "new")]);
    assert_eq!(report.diff.added, [root("add", "add", "add")]);
    assert_eq!(report.diff.unchanged, [root("keep", "keep", "keep")]);
    assert_eq!(report.removed_stale_roots, ["/nix/store/old"]);
    assert_eq!(
        *backend.events.borrow(),
        [
            "verify:/nix/store/keep",
            "realize:/nix/store/shared.drv",
            "outputs:/nix/store/add,/nix/store/new",
            "closure:/nix/store/add",
            "closure:/nix/store/new",
            "publish:/nix/store/add,/nix/store/dependency,/nix/store/new",
            "verify:/nix/store/add,/nix/store/dependency,/nix/store/keep,/nix/store/new",
            "record:/nix/store/keep,/nix/store/new,/nix/store/add",
            "remove:/nix/store/old",
        ],
    );
}

#[test]
fn missing_cache_paths_and_failed_receipts_block_cleanup() {
    for backend in [
        Backend {
            missing: vec!["/nix/store/dependency".into()],
            ..Backend::default()
        },
        Backend {
            fail_receipt: true,
            ..Backend::default()
        },
    ] {
        let error =
            prepare_native_recovery(&artifact(), &previous_roots(), &backend, &backend, &backend)
                .unwrap_err();
        let events = backend.events.borrow();
        assert!(events.iter().any(|event| event.starts_with("publish:")));
        assert!(!events.iter().any(|event| event.starts_with("remove:")));
        if backend.fail_receipt {
            assert!(matches!(error, Error::CommandFailed { .. }));
        } else {
            assert!(matches!(error, Error::MissingCachePaths { count: 1, .. }));
            assert!(!events.iter().any(|event| event.starts_with("record:")));
        }
    }
}

#[test]
fn missing_unchanged_root_is_retried() {
    let backend = Backend {
        missing: vec!["/nix/store/keep".into()],
        ..Backend::default()
    };
    let error =
        prepare_native_recovery(&artifact(), &previous_roots(), &backend, &backend, &backend)
            .unwrap_err();
    assert!(matches!(error, Error::MissingCachePaths { count: 1, .. }));
    assert!(
        backend
            .events
            .borrow()
            .iter()
            .any(|event| event == "realize:/nix/store/keep.drv,/nix/store/shared.drv")
    );
}

#[test]
fn a_path_change_requires_recovery_even_if_the_fingerprint_matches() {
    let backend = Backend::default();
    let mut previous = previous_roots();
    previous[0].path = "/nix/store/stale-keep".into();
    let report =
        prepare_native_recovery(&artifact(), &previous, &backend, &backend, &backend).unwrap();
    assert!(report.diff.changed.iter().any(|root| root.label == "keep"));
    assert!(report.published_paths.contains(&"/nix/store/keep".into()));
    assert!(
        report
            .removed_stale_roots
            .contains(&"/nix/store/stale-keep".into())
    );
}

#[test]
fn removal_only_records_the_empty_state_before_releasing_roots() {
    let backend = Backend::default();
    let empty = RequirementsArtifact {
        roots: vec![],
        drvs: vec![],
        root_labels: vec![],
        root_fingerprints: vec![],
        fingerprint: None,
    };
    prepare_native_recovery(&empty, &previous_roots(), &backend, &backend, &backend).unwrap();
    assert_eq!(
        *backend.events.borrow(),
        ["record:", "remove:/nix/store/keep", "remove:/nix/store/old"]
    );
}
