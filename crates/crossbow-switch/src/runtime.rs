//! Planning and verification of cached runtime references across stores.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// One exact cached runtime path, recorded in dependency-first restore order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimePath {
    /// Absolute store path to validate or restore.
    pub path: String,
    /// Planned source store, or `None` for a path already in the local daemon.
    pub substituter: Option<String>,
    /// NAR hash advertised by the selected store when the plan was sealed.
    pub nar_hash: String,
    /// Exact direct runtime references, including any self-reference.
    pub references: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PathInfo {
    #[serde(rename = "narHash")]
    pub nar_hash: String,
    pub references: Vec<String>,
}

type StoreInfo = BTreeMap<String, Option<PathInfo>>;

pub(crate) fn query_store(store: &str, paths: &[String]) -> Result<StoreInfo> {
    if paths.is_empty() {
        return Ok(StoreInfo::new());
    }
    // Local metadata comes directly from the daemon; a remote ping separates
    // an unavailable cache from an ordinary per-path miss.
    if store != "daemon" {
        let ping = Command::new("nix")
            .args(["store", "ping", "--store", store])
            .output()
            .map_err(|source| Error::PlannerCacheProbe {
                substituter: store.to_owned(),
                output: paths.first().cloned().unwrap_or_default(),
                source,
            })?;
        if !ping.status.success() {
            return Err(Error::PlannerCacheProbeFailed {
                substituter: store.to_owned(),
                output: paths.first().cloned().unwrap_or_default(),
                stderr: String::from_utf8_lossy(&ping.stderr).trim().to_owned(),
            });
        }
    }
    let mut result = StoreInfo::new();
    for chunk in paths.chunks(1024) {
        let output = Command::new("nix")
            .args([
                "path-info",
                "--no-allow-import-from-derivation",
                "--json",
                "--json-format",
                "1",
                "--store",
                store,
            ])
            .args(chunk)
            .output()
            .map_err(|source| Error::PlannerCacheProbe {
                substituter: store.to_owned(),
                output: chunk[0].clone(),
                source,
            })?;
        // A complete JSON map, including null entries for misses, remains
        // authoritative when path-info exits nonzero for an ordinary miss.
        let values: StoreInfo = serde_json::from_slice(&output.stdout).map_err(|error| {
            Error::PlannerCacheProbeJson {
                substituter: store.to_owned(),
                output: chunk[0].clone(),
                reason: error.to_string(),
            }
        })?;
        for path in chunk {
            let value = values
                .get(path)
                .ok_or_else(|| Error::PlannerCacheProbeJson {
                    substituter: store.to_owned(),
                    output: path.clone(),
                    reason: "response omitted the queried runtime path".to_owned(),
                })?;
            result.insert(path.clone(), value.clone());
        }
    }
    Ok(result)
}

pub(crate) fn plan_runtime_paths(
    roots: &BTreeMap<String, String>,
    substituters: &[String],
) -> Result<Vec<RuntimePath>> {
    plan_with_probe(roots, substituters, &query_store)
}

fn plan_with_probe(
    roots: &BTreeMap<String, String>,
    substituters: &[String],
    probe: &(impl Fn(&str, &[String]) -> Result<StoreInfo> + Sync),
) -> Result<Vec<RuntimePath>> {
    let mut seen_stores = BTreeSet::new();
    let stores = std::iter::once("daemon".to_owned())
        .chain(substituters.iter().cloned())
        .filter(|store| seen_stores.insert(store.clone()))
        .collect::<Vec<_>>();
    if let Some((path, _)) = roots.iter().find(|(_, store)| !stores.contains(store)) {
        return Err(blocked(path, "output origin is not a declared substituter"));
    }
    let mut pending = roots.keys().cloned().collect::<BTreeSet<_>>();
    let mut paths = BTreeMap::<String, RuntimePath>::new();
    while !pending.is_empty() {
        let batch = pending.iter().cloned().collect::<Vec<_>>();
        let local = probe("daemon", &batch);
        // Runtime closures often consist mostly of already-local paths. Keep
        // those references out of remote batches while rechecking pinned root
        // metadata against its exact recorded origin.
        let mut probes = std::thread::scope(|scope| {
            let handles = stores
                .iter()
                .filter(|store| store.as_str() != "daemon")
                .filter_map(|store| {
                    let remote_paths = batch
                        .iter()
                        .filter(|path| {
                            roots.get(*path).map_or_else(
                                || {
                                    local
                                        .as_ref()
                                        .ok()
                                        .and_then(|info| info.get(*path))
                                        .is_none_or(Option::is_none)
                                },
                                |origin| origin == store,
                            )
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    (!remote_paths.is_empty())
                        .then(|| (store, scope.spawn(move || probe(store, &remote_paths))))
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|(store, handle)| {
                    let result = handle.join().unwrap_or_else(|_| {
                        Err(blocked(&batch[0], "runtime metadata probe panicked"))
                    });
                    (store, result)
                })
                .collect::<Vec<_>>()
        });
        probes.insert(0, (&stores[0], local));
        pending.clear();
        for path in batch {
            let hit = probes.iter().find_map(|(store, result)| {
                if roots.get(&path).is_some_and(|origin| origin != *store) {
                    return None;
                }
                let info = result.as_ref().ok()?.get(&path)?.as_ref()?;
                Some(((*store).clone(), info))
            });
            let Some((store, info)) = hit else {
                let failures = probes
                    .iter()
                    .filter(|(store, result)| {
                        result.is_err() && roots.get(&path).is_none_or(|origin| origin == *store)
                    })
                    .map(|(store, _)| store.as_str())
                    .collect::<Vec<_>>();
                return Err(blocked(
                    &path,
                    &if failures.is_empty() {
                        "runtime reference is absent from its eligible stores".to_owned()
                    } else {
                        format!(
                            "runtime reference availability is indeterminate: {}",
                            failures.join(", ")
                        )
                    },
                ));
            };
            let references = info
                .references
                .iter()
                .map(|reference| {
                    if reference.starts_with("/nix/store/") {
                        reference.clone()
                    } else {
                        format!("/nix/store/{reference}")
                    }
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let entry = RuntimePath {
                path: path.clone(),
                substituter: (store != "daemon").then_some(store),
                nar_hash: info.nar_hash.clone(),
                references,
            };
            validate_path(&entry)?;
            pending.extend(
                entry
                    .references
                    .iter()
                    .filter(|reference| *reference != &path && !paths.contains_key(*reference))
                    .cloned(),
            );
            paths.insert(path, entry);
        }
        pending.retain(|path| !paths.contains_key(path));
    }
    dependency_first(&paths)
}

pub(crate) fn blocked(path: &str, reason: &str) -> Error {
    Error::PlanBlocked {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

pub(crate) fn validate_path(path: &RuntimePath) -> Result<()> {
    let valid = |value: &str| {
        value.strip_prefix("/nix/store/").is_some_and(|name| {
            !name.is_empty()
                && !name.contains('/')
                && !name.chars().any(char::is_whitespace)
                && name != "."
                && name != ".."
        })
    };
    if !valid(&path.path)
        || path.references.iter().any(|reference| !valid(reference))
        || path.nar_hash.is_empty()
        || path.substituter.as_ref().is_some_and(String::is_empty)
    {
        return Err(blocked(&path.path, "invalid runtime path metadata"));
    }
    Ok(())
}

fn dependency_first(paths: &BTreeMap<String, RuntimePath>) -> Result<Vec<RuntimePath>> {
    fn visit(
        path: &str,
        paths: &BTreeMap<String, RuntimePath>,
        visiting: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
        ordered: &mut Vec<RuntimePath>,
    ) -> Result<()> {
        if done.contains(path) {
            return Ok(());
        }
        if !visiting.insert(path.to_owned()) {
            return Err(blocked(
                path,
                "cached runtime references contain a non-self cycle",
            ));
        }
        let entry = paths
            .get(path)
            .ok_or_else(|| blocked(path, "runtime reference was not planned"))?;
        for reference in &entry.references {
            if reference != path {
                visit(reference, paths, visiting, done, ordered)?;
            }
        }
        visiting.remove(path);
        done.insert(path.to_owned());
        ordered.push(entry.clone());
        Ok(())
    }
    let mut ordered = Vec::with_capacity(paths.len());
    let mut visiting = BTreeSet::new();
    let mut done = BTreeSet::new();
    for path in paths.keys() {
        visit(path, paths, &mut visiting, &mut done, &mut ordered)?;
    }
    Ok(ordered)
}

pub(crate) fn validate_schedule(paths: &[RuntimePath]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for path in paths {
        validate_path(path)?;
        if !seen.insert(path.path.clone()) {
            return Err(blocked(&path.path, "runtime path appears more than once"));
        }
        if path
            .references
            .iter()
            .any(|reference| reference != &path.path && !seen.contains(reference))
        {
            return Err(blocked(
                &path.path,
                "runtime references are not in dependency-first order",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> BTreeMap<(String, String), PathInfo> {
        BTreeMap::from([
            (
                ("review".into(), "/nix/store/root".into()),
                PathInfo {
                    nar_hash: "sha256-root".into(),
                    references: vec!["/nix/store/root".into(), "/nix/store/leaf".into()],
                },
            ),
            (
                ("upstream".into(), "/nix/store/leaf".into()),
                PathInfo {
                    nar_hash: "sha256-leaf".into(),
                    references: vec![],
                },
            ),
        ])
    }

    fn plan_fixture(
        entries: &BTreeMap<(String, String), PathInfo>,
        failed_store: Option<&str>,
    ) -> Result<Vec<RuntimePath>> {
        plan_with_probe(
            &BTreeMap::from([("/nix/store/root".into(), "review".into())]),
            &["review".into(), "upstream".into(), "unavailable".into()],
            &|store, paths| {
                if failed_store == Some(store) {
                    return Err(blocked(&paths[0], "store unavailable"));
                }
                Ok(paths
                    .iter()
                    .map(|path| {
                        (
                            path.clone(),
                            entries.get(&(store.into(), path.clone())).cloned(),
                        )
                    })
                    .collect())
            },
        )
    }

    #[test]
    fn split_cache_closure_keeps_exact_origins_and_self_references() {
        let paths = plan_fixture(&fixture(), Some("unavailable")).unwrap();
        assert_eq!(
            paths
                .iter()
                .map(|path| path.path.as_str())
                .collect::<Vec<_>>(),
            ["/nix/store/leaf", "/nix/store/root"]
        );
        assert_eq!(paths[0].substituter.as_deref(), Some("upstream"));
        assert_eq!(paths[1].substituter.as_deref(), Some("review"));
        assert!(paths[1].references.contains(&"/nix/store/root".into()));
        validate_schedule(&paths).unwrap();
    }

    #[test]
    fn local_reference_is_validated_without_changing_the_root_origin() {
        let mut entries = fixture();
        let leaf = entries
            .remove(&("upstream".into(), "/nix/store/leaf".into()))
            .unwrap();
        entries.insert(("daemon".into(), "/nix/store/leaf".into()), leaf);
        let root = entries[&("review".into(), "/nix/store/root".into())].clone();
        entries.insert(("daemon".into(), "/nix/store/root".into()), root);
        let paths = plan_fixture(&entries, None).unwrap();
        assert_eq!(paths[0].substituter, None);
        assert_eq!(paths[1].substituter.as_deref(), Some("review"));
    }

    #[test]
    fn already_local_references_are_not_probed_in_remote_stores() {
        let entries = fixture();
        let local_leaf = entries[&("upstream".into(), "/nix/store/leaf".into())].clone();
        let paths = plan_with_probe(
            &BTreeMap::from([("/nix/store/root".into(), "review".into())]),
            &["review".into(), "upstream".into()],
            &|store, paths| {
                if store != "daemon" {
                    assert_eq!(paths, ["/nix/store/root"]);
                }
                Ok(paths
                    .iter()
                    .map(|path| {
                        let info = if store == "daemon" && path == "/nix/store/leaf" {
                            Some(local_leaf.clone())
                        } else {
                            entries.get(&(store.into(), path.clone())).cloned()
                        };
                        (path.clone(), info)
                    })
                    .collect())
            },
        )
        .unwrap();
        assert_eq!(paths[0].substituter, None);
    }

    #[test]
    fn real_nix_runtime_metadata_matches_the_typed_contract() {
        let Some(nix) = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("nix"))
                .find_map(|binary| binary.canonicalize().ok())
        }) else {
            return;
        };
        let Some(store_path) = nix.ancestors().find(|path| {
            path.parent()
                .is_some_and(|parent| parent == std::path::Path::new("/nix/store"))
        }) else {
            return;
        };
        let store_path = store_path.to_string_lossy().into_owned();
        let missing = "/nix/store/00000000000000000000000000000000-crossbow-invalid".to_owned();
        let info = query_store("daemon", &[store_path.clone(), missing.clone()]).unwrap();
        assert!(info[&missing].is_none());
        let present = info[&store_path].as_ref().unwrap();
        assert!(present.nar_hash.starts_with("sha256-"));
        assert!(!present.references.is_empty());
        validate_path(&RuntimePath {
            path: store_path,
            substituter: None,
            nar_hash: present.nar_hash.clone(),
            references: present.references.clone(),
        })
        .unwrap();
    }

    #[test]
    fn missing_runtime_reference_fails_before_execution() {
        let mut entries = fixture();
        entries.remove(&("upstream".into(), "/nix/store/leaf".into()));
        let error = plan_fixture(&entries, None).unwrap_err();
        assert!(matches!(error, Error::PlanBlocked { path, reason }
            if path == "/nix/store/leaf" && reason.contains("absent")));
        let error = plan_fixture(&entries, Some("upstream")).unwrap_err();
        assert!(matches!(error, Error::PlanBlocked { reason, .. }
            if reason.contains("indeterminate")));
    }

    #[test]
    fn root_metadata_must_come_from_its_recorded_origin() {
        let mut entries = fixture();
        let root = entries
            .remove(&("review".into(), "/nix/store/root".into()))
            .unwrap();
        entries.insert(("upstream".into(), "/nix/store/root".into()), root);
        assert!(
            matches!(plan_fixture(&entries, None), Err(Error::PlanBlocked { path, .. })
            if path == "/nix/store/root")
        );
    }

    #[test]
    fn cyclic_or_invalid_cache_metadata_fails_closed() {
        let mut entries = fixture();
        entries
            .get_mut(&("upstream".into(), "/nix/store/leaf".into()))
            .unwrap()
            .references
            .push("/nix/store/root".into());
        assert!(
            matches!(plan_fixture(&entries, None), Err(Error::PlanBlocked { reason, .. })
            if reason.contains("cycle"))
        );
        let mut entries = fixture();
        entries
            .get_mut(&("review".into(), "/nix/store/root".into()))
            .unwrap()
            .references
            .push("/nix/store/../outside".into());
        assert!(
            matches!(plan_fixture(&entries, None), Err(Error::PlanBlocked { reason, .. })
            if reason.contains("invalid"))
        );
    }
}
