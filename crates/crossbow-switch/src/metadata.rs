use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Error, Result};

const METADATA_JSON: &str = include_str!("../data/crossbow-metadata.json");

/// Complete Crossbow metadata loaded from `data/crossbow-metadata.json`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    /// Canonical host target descriptors keyed by Crossbow target name.
    pub targets: BTreeMap<String, TargetDescriptor>,
    /// Nix-system to compiler-target mappings.
    pub platform_mappings: PlatformMappings,
    /// Named cache strategy descriptors.
    pub cache_modes: BTreeMap<String, CacheMode>,
    /// Named hardware optimization profiles.
    pub hardware_profiles: BTreeMap<String, HardwareProfile>,
    /// Named build optimization profiles.
    pub build_optimization_profiles: BTreeMap<String, BuildOptimizationProfile>,
}

/// Nix target descriptor used by Crossbow public metadata APIs.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TargetDescriptor {
    /// Nix system string for the target.
    pub system: String,
    /// GNU-style target config string.
    pub config: String,
}

/// Compiler target mappings keyed by Nix system string.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformMappings {
    /// Zig target triples keyed by Nix system string.
    pub zig_targets: BTreeMap<String, String>,
    /// GNU config triples keyed by Nix system string.
    pub gnu_configs: BTreeMap<String, String>,
}

/// Public descriptor for a Crossbow cache mode.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheMode {
    /// Stable cache mode name.
    pub name: String,
    /// What the mode proves operationally.
    pub proves: String,
    /// Expected binary-cache reuse behavior.
    pub cache_expectation: String,
}

/// Hardware-specific compiler tuning profile.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HardwareProfile {
    /// Stable profile name.
    pub name: String,
    /// Nix system the profile applies to, when constrained.
    pub system: Option<String>,
    /// Hardware vendor name, when known.
    pub vendor: Option<String>,
    /// System-on-chip identifier, when known.
    pub soc: Option<String>,
    /// Human-readable profile description.
    pub description: Option<String>,
    /// Platform metadata merged into the host platform.
    pub platform: Option<PlatformMetadata>,
}

/// Partial Nix platform metadata carried by hardware profiles.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PlatformMetadata {
    /// GCC/Clang architecture and tuning metadata.
    pub gcc: Option<GccMetadata>,
}

/// GCC/Clang architecture and tune settings.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GccMetadata {
    /// `-march` value represented as nixpkgs platform metadata.
    pub arch: Option<String>,
    /// `-mtune` value represented as nixpkgs platform metadata.
    pub tune: Option<String>,
}

/// Build optimization profile shared between Nix and Rust validation.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildOptimizationProfile {
    /// Stable profile name.
    pub name: String,
    /// Whether applying the profile changes derivation hashes.
    pub changes_hashes: bool,
    /// C compiler flags.
    pub c_flags: Vec<String>,
    /// Linker flags.
    pub link_flags: Vec<String>,
    /// Rust compiler flags.
    pub rust_flags: Vec<String>,
    /// Go compiler flags.
    pub go_flags: Vec<String>,
    /// Go linker flags.
    pub go_ldflags: Vec<String>,
    /// Go environment overrides.
    pub go_env: BTreeMap<String, String>,
    /// Human-readable profile description.
    pub description: String,
}

/// Hardware optimization input accepted by Rust helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareOptimization<'a> {
    /// Lookup a named profile from [`Metadata::hardware_profiles`].
    Named(&'a str),
    /// Use a caller-provided custom profile.
    Custom(HardwareProfile),
}

/// Host platform after applying optional hardware optimization metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizedHostPlatform {
    /// Nix system string for the optimized host platform.
    pub system: String,
    /// GNU-style target config string preserved from the host target.
    pub config: String,
    /// Optional GCC/Clang platform tuning metadata.
    pub gcc: Option<GccMetadata>,
}

/// Loads embedded Crossbow metadata.
///
/// # Errors
///
/// Returns [`Error::MetadataJson`] if the embedded JSON cannot be parsed.
pub fn metadata() -> Result<Metadata> {
    serde_json::from_str(METADATA_JSON).map_err(Into::into)
}

/// Looks up a target descriptor by Crossbow target name.
///
/// # Errors
///
/// Returns [`Error::UnsupportedHostPlatform`] when `system` is not present in
/// [`Metadata::targets`].
pub fn target_for<'a>(metadata: &'a Metadata, system: &str) -> Result<&'a TargetDescriptor> {
    metadata
        .targets
        .get(system)
        .ok_or_else(|| Error::UnsupportedHostPlatform {
            system: system.to_owned(),
        })
}

/// Looks up the Zig target triple for a Nix system string.
///
/// # Errors
///
/// Returns [`Error::MissingPlatformMapping`] if no Zig mapping exists.
pub fn nix_system_to_zig_target(metadata: &Metadata, system: &str) -> Result<String> {
    lookup_platform_mapping(
        "Zig target",
        &metadata.platform_mappings.zig_targets,
        system,
    )
}

/// Looks up the GNU config triple for a Nix system string.
///
/// # Errors
///
/// Returns [`Error::MissingPlatformMapping`] if no GNU config mapping exists.
pub fn nix_system_to_gnu_config(metadata: &Metadata, system: &str) -> Result<String> {
    lookup_platform_mapping(
        "GNU config",
        &metadata.platform_mappings.gnu_configs,
        system,
    )
}

/// Looks up a named cache mode.
///
/// # Errors
///
/// Returns [`Error::UnsupportedCacheMode`] if `mode` is unknown.
pub fn cache_mode_for<'a>(metadata: &'a Metadata, mode: &str) -> Result<&'a CacheMode> {
    metadata
        .cache_modes
        .get(mode)
        .ok_or_else(|| Error::UnsupportedCacheMode {
            mode: mode.to_owned(),
            expected: metadata
                .cache_modes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// Resolves optional hardware optimization input to a profile.
///
/// # Errors
///
/// Returns [`Error::UnsupportedHardwareProfile`] when a named profile is not
/// present in [`Metadata::hardware_profiles`].
pub fn hardware_profile_for<'a>(
    metadata: &'a Metadata,
    hardware_optimization: Option<HardwareOptimization<'a>>,
) -> Result<Option<HardwareProfile>> {
    match hardware_optimization {
        None => Ok(None),
        Some(HardwareOptimization::Custom(profile)) => Ok(Some(profile)),
        Some(HardwareOptimization::Named(name)) => metadata
            .hardware_profiles
            .get(name)
            .cloned()
            .map(Some)
            .ok_or_else(|| Error::UnsupportedHardwareProfile {
                profile: name.to_owned(),
                expected: metadata
                    .hardware_profiles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
    }
}

/// Returns the display name for optional hardware optimization input.
///
/// # Errors
///
/// Returns [`Error::UnsupportedHardwareProfile`] when a named profile is not
/// present in [`Metadata::hardware_profiles`].
pub fn hardware_optimization_name(
    metadata: &Metadata,
    hardware_optimization: Option<HardwareOptimization<'_>>,
) -> Result<Option<String>> {
    Ok(hardware_profile_for(metadata, hardware_optimization)?.map(|profile| profile.name))
}

/// Merges a hardware profile into a host platform descriptor.
///
/// # Errors
///
/// Returns [`Error::UnsupportedHostPlatform`] when `host` is unknown,
/// [`Error::UnsupportedHardwareProfile`] when a named profile is unknown, or
/// [`Error::HardwareProfileHostMismatch`] when the profile targets another
/// system.
pub fn optimized_host_platform(
    metadata: &Metadata,
    host: &str,
    hardware_optimization: Option<HardwareOptimization<'_>>,
) -> Result<OptimizedHostPlatform> {
    let target = target_for(metadata, host)?;
    let Some(profile) = hardware_profile_for(metadata, hardware_optimization)? else {
        return Ok(OptimizedHostPlatform {
            system: host.to_owned(),
            config: target.config.clone(),
            gcc: None,
        });
    };

    if profile.system.as_deref().unwrap_or(host) != host {
        return Err(Error::HardwareProfileHostMismatch {
            profile: profile.name,
            profile_system: profile.system.unwrap_or_else(|| "<unknown>".to_owned()),
            host: host.to_owned(),
        });
    }

    Ok(OptimizedHostPlatform {
        system: host.to_owned(),
        config: target.config.clone(),
        gcc: profile.platform.and_then(|platform| platform.gcc),
    })
}

/// Looks up a build optimization profile, defaulting to `cache-first`.
///
/// # Errors
///
/// Returns [`Error::UnsupportedBuildOptimizationProfile`] when `profile` names
/// an unknown profile.
pub fn build_optimization_profile_for<'a>(
    metadata: &'a Metadata,
    profile: Option<&str>,
) -> Result<&'a BuildOptimizationProfile> {
    let name = profile.unwrap_or("cache-first");
    metadata
        .build_optimization_profiles
        .get(name)
        .ok_or_else(|| Error::UnsupportedBuildOptimizationProfile {
            profile: name.to_owned(),
            expected: metadata
                .build_optimization_profiles
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// Returns the public unsupported-toolchain message for a build/host pair.
#[must_use]
pub fn unsupported_toolchain_message(build: &str, host: &str, darwin_sdk: Option<&str>) -> String {
    if is_wasm(host) {
        "crossbow: wasm toolchain is declared but not implemented in phase 1".to_owned()
    } else if is_windows(host) {
        "crossbow: windows toolchain is declared but not implemented in phase 1".to_owned()
    } else if is_linux(build) && is_darwin(host) && darwin_sdk.is_none() {
        "crossbow: linux-to-darwin cross-compilation requires darwinSdk.\nSet darwinSdk = /path/to/MacOSX.sdk and configure osxcross.\n".to_owned()
    } else if is_darwin(host) {
        "crossbow: darwin toolchain is declared but not implemented in phase 1".to_owned()
    } else {
        format!("crossbow: unsupported toolchain pair `{build}` -> `{host}`")
    }
}

/// Returns the operating-system suffix from a Nix system string.
#[must_use]
pub fn os_of(system: &str) -> Option<&str> {
    system.rsplit('-').next()
}

/// Returns true when `system` is a Linux Nix system.
#[must_use]
pub fn is_linux(system: &str) -> bool {
    os_of(system) == Some("linux")
}

/// Returns true when `system` is a Darwin Nix system.
#[must_use]
pub fn is_darwin(system: &str) -> bool {
    os_of(system) == Some("darwin")
}

/// Returns true when `system` is a Windows Nix system.
#[must_use]
pub fn is_windows(system: &str) -> bool {
    os_of(system) == Some("windows")
}

/// Returns true when `system` is a WebAssembly Nix system.
#[must_use]
pub fn is_wasm(system: &str) -> bool {
    system.starts_with("wasm")
}

fn lookup_platform_mapping(
    name: &str,
    table: &BTreeMap<String, String>,
    system: &str,
) -> Result<String> {
    table
        .get(system)
        .cloned()
        .ok_or_else(|| Error::MissingPlatformMapping {
            name: name.to_owned(),
            system: system.to_owned(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_metadata_loads_expected_public_values() -> Result<()> {
        let metadata = metadata()?;

        assert_eq!(
            target_for(&metadata, "aarch64-linux")?,
            &TargetDescriptor {
                system: "aarch64-linux".to_owned(),
                config: "aarch64-unknown-linux-gnu".to_owned(),
            }
        );
        assert_eq!(
            nix_system_to_zig_target(&metadata, "aarch64-linux")?,
            "aarch64-linux-gnu"
        );
        assert_eq!(
            nix_system_to_gnu_config(&metadata, "aarch64-linux")?,
            "aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            cache_mode_for(&metadata, "cache-shaped-with-cross-overrides")?.name,
            "cache-shaped-with-cross-overrides"
        );
        assert_eq!(
            build_optimization_profile_for(&metadata, Some("fast-local"))?.rust_flags,
            vec!["-C", "lto=thin", "-C", "link-arg=-fuse-ld=mold"]
        );

        Ok(())
    }

    #[test]
    fn linux_to_darwin_without_sdk_mentions_darwin_sdk() {
        let message = unsupported_toolchain_message("x86_64-linux", "aarch64-darwin", None);

        assert!(message.contains("darwinSdk"));
        assert!(message.contains("osxcross"));
    }

    #[test]
    fn metadata_lookup_errors_include_offending_value() -> Result<()> {
        let metadata = metadata()?;

        let target = target_for(&metadata, "mips-linux").unwrap_err();
        let zig = nix_system_to_zig_target(&metadata, "mips-linux").unwrap_err();
        let cache_mode = cache_mode_for(&metadata, "mystery-cache").unwrap_err();

        assert!(target.to_string().contains("mips-linux"));
        assert!(zig.to_string().contains("mips-linux"));
        assert!(cache_mode.to_string().contains("mystery-cache"));
        assert!(cache_mode.to_string().contains("strict-cross"));
        Ok(())
    }

    #[test]
    fn build_optimization_profile_defaults_to_cache_first() -> Result<()> {
        let metadata = metadata()?;

        assert_eq!(
            build_optimization_profile_for(&metadata, None)?.name,
            "cache-first"
        );
        Ok(())
    }

    #[test]
    fn hardware_optimization_name_handles_absent_and_named_profiles() -> Result<()> {
        let metadata = metadata()?;

        assert_eq!(hardware_optimization_name(&metadata, None)?, None);
        assert_eq!(
            hardware_optimization_name(&metadata, Some(HardwareOptimization::Named("rockpro64")))?,
            Some("rockpro64".to_owned())
        );
        Ok(())
    }

    #[test]
    fn system_classifiers_follow_nix_system_suffixes() {
        assert_eq!(os_of("x86_64-linux"), Some("linux"));
        assert!(is_linux("x86_64-linux"));
        assert!(is_darwin("aarch64-darwin"));
        assert!(is_windows("x86_64-windows"));
        assert!(is_wasm("wasm32-wasi"));
        assert!(!is_linux("wasm32-wasi"));
    }

    #[test]
    fn incompatible_hardware_profile_reports_host_mismatch() -> Result<()> {
        let metadata = metadata()?;
        let error = optimized_host_platform(
            &metadata,
            "x86_64-linux",
            Some(HardwareOptimization::Named("rockpro64")),
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("is for `aarch64-linux` but host is `x86_64-linux`")
        );
        Ok(())
    }

    #[test]
    fn optimized_host_platform_merges_gcc_profile() -> Result<()> {
        let metadata = metadata()?;
        let platform = optimized_host_platform(
            &metadata,
            "aarch64-linux",
            Some(HardwareOptimization::Named("rockpro64")),
        )?;

        assert_eq!(platform.system, "aarch64-linux");
        assert_eq!(platform.config, "aarch64-unknown-linux-gnu");
        assert_eq!(
            platform.gcc,
            Some(GccMetadata {
                arch: Some("armv8-a".to_owned()),
                tune: Some("cortex-a72.cortex-a53".to_owned()),
            })
        );
        Ok(())
    }
}
