use crate::{Error, Result};

/// Executor descriptor accepted by Crossbow check planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorDescriptor {
    /// Check by requiring a native builder for the host system.
    NativeBuilder {
        /// Optional builder names accepted by the descriptor.
        builders: Vec<String>,
    },
    /// Mark the check as intentionally skipped with a human-readable reason.
    Skip {
        /// Human-readable skip reason.
        reason: String,
    },
    /// Declared WASI executor stub.
    Wasmtime {
        /// Whether a concrete wasmtime package was supplied by the caller.
        configured: bool,
    },
    /// Declared Windows/Wine executor stub.
    Wine {
        /// Whether a concrete wine package was supplied by the caller.
        configured: bool,
    },
}

/// Planned check behavior derived from an [`ExecutorDescriptor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckPlan {
    /// Native-builder checks record the system that must be executable.
    NativeBuilder {
        /// Host system required by the check.
        required_system: String,
    },
    /// Skipped checks emit the supplied reason.
    Skipped {
        /// Human-readable skip reason.
        reason: String,
    },
}

/// Validates an executor descriptor for a package check.
///
/// # Errors
///
/// Returns [`Error::NativeBuilderMissingHost`] when a native-builder check has
/// no host system, and [`Error::ExecutorNotImplemented`] for declared stubs.
pub fn plan_executor_check(
    executor: &ExecutorDescriptor,
    host_system: &str,
    check_name: &str,
) -> Result<CheckPlan> {
    match executor {
        ExecutorDescriptor::Skip { reason } => Ok(CheckPlan::Skipped {
            reason: reason.clone(),
        }),
        ExecutorDescriptor::NativeBuilder { .. } => {
            if host_system.is_empty() {
                return Err(Error::NativeBuilderMissingHost {
                    check_name: check_name.to_owned(),
                });
            }

            Ok(CheckPlan::NativeBuilder {
                required_system: host_system.to_owned(),
            })
        }
        ExecutorDescriptor::Wasmtime { .. } => Err(Error::ExecutorNotImplemented {
            kind: "wasmtime".to_owned(),
        }),
        ExecutorDescriptor::Wine { .. } => Err(Error::ExecutorNotImplemented {
            kind: "wine".to_owned(),
        }),
    }
}

/// Parses an executor kind name into an [`ExecutorDescriptor`].
///
/// # Errors
///
/// Returns [`Error::UnknownExecutor`] for names Crossbow does not recognize.
pub fn parse_executor_kind(kind: &str) -> Result<ExecutorDescriptor> {
    match kind {
        "native-builder" => Ok(ExecutorDescriptor::NativeBuilder {
            builders: Vec::new(),
        }),
        "skip" => Ok(ExecutorDescriptor::Skip {
            reason: "crossbow check skipped".to_owned(),
        }),
        "wasmtime" => Ok(ExecutorDescriptor::Wasmtime { configured: false }),
        "wine" => Ok(ExecutorDescriptor::Wine { configured: false }),
        other => Err(Error::UnknownExecutor {
            kind: other.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_builder_requires_host_system() {
        let error = plan_executor_check(
            &ExecutorDescriptor::NativeBuilder {
                builders: Vec::new(),
            },
            "",
            "tiny",
        )
        .unwrap_err();

        assert!(error.to_string().contains("non-empty host system"));
    }

    #[test]
    fn native_builder_returns_required_system() -> Result<()> {
        let plan = plan_executor_check(
            &ExecutorDescriptor::NativeBuilder {
                builders: Vec::new(),
            },
            "aarch64-linux",
            "tiny",
        )?;

        assert_eq!(
            plan,
            CheckPlan::NativeBuilder {
                required_system: "aarch64-linux".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn skip_executor_preserves_reason() -> Result<()> {
        let plan = plan_executor_check(
            &ExecutorDescriptor::Skip {
                reason: "no runner".to_owned(),
            },
            "wasm32-wasi",
            "tiny",
        )?;

        assert_eq!(
            plan,
            CheckPlan::Skipped {
                reason: "no runner".to_owned(),
            }
        );
        Ok(())
    }

    #[test]
    fn parse_executor_kind_accepts_known_kinds() -> Result<()> {
        assert_eq!(
            parse_executor_kind("native-builder")?,
            ExecutorDescriptor::NativeBuilder {
                builders: Vec::new(),
            }
        );
        assert_eq!(
            parse_executor_kind("skip")?,
            ExecutorDescriptor::Skip {
                reason: "crossbow check skipped".to_owned(),
            }
        );
        assert_eq!(
            parse_executor_kind("wasmtime")?,
            ExecutorDescriptor::Wasmtime { configured: false }
        );
        assert_eq!(
            parse_executor_kind("wine")?,
            ExecutorDescriptor::Wine { configured: false }
        );
        Ok(())
    }

    #[test]
    fn parse_executor_kind_rejects_unknown_kind() {
        let error = parse_executor_kind("qemu").unwrap_err();

        assert!(error.to_string().contains("unknown executor `qemu`"));
    }

    #[test]
    fn declared_stub_executors_report_phase_1_error() {
        let wasmtime = plan_executor_check(
            &ExecutorDescriptor::Wasmtime { configured: false },
            "x",
            "tiny",
        )
        .unwrap_err();
        let wine =
            plan_executor_check(&ExecutorDescriptor::Wine { configured: false }, "x", "tiny")
                .unwrap_err();

        assert!(wasmtime.to_string().contains("not implemented in phase 1"));
        assert!(wine.to_string().contains("not implemented in phase 1"));
    }
}
