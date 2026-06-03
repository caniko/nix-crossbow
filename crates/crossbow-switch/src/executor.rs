use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutorDescriptor {
    NativeBuilder { builders: Vec<String> },
    Skip { reason: String },
    Wasmtime { configured: bool },
    Wine { configured: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckPlan {
    NativeBuilder { required_system: String },
    Skipped { reason: String },
}

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
