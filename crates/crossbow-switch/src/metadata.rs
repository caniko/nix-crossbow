use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Error, Result};

const METADATA_JSON: &str = include_str!("../../../data/crossbow-metadata.json");

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub targets: BTreeMap<String, TargetDescriptor>,
    pub platform_mappings: PlatformMappings,
    pub cache_modes: BTreeMap<String, CacheMode>,
    pub hardware_profiles: BTreeMap<String, HardwareProfile>,
    pub build_optimization_profiles: BTreeMap<String, BuildOptimizationProfile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TargetDescriptor {
    pub system: String,
    pub config: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformMappings {
    pub zig_targets: BTreeMap<String, String>,
    pub gnu_configs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CacheMode {
    pub name: String,
    pub proves: String,
    pub cache_expectation: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct HardwareProfile {
    pub name: String,
    pub system: Option<String>,
    pub vendor: Option<String>,
    pub soc: Option<String>,
    pub description: Option<String>,
    pub platform: Option<PlatformMetadata>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PlatformMetadata {
    pub gcc: Option<GccMetadata>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct GccMetadata {
    pub arch: Option<String>,
    pub tune: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BuildOptimizationProfile {
    pub name: String,
    pub changes_hashes: bool,
    pub c_flags: Vec<String>,
    pub link_flags: Vec<String>,
    pub rust_flags: Vec<String>,
    pub go_flags: Vec<String>,
    pub go_ldflags: Vec<String>,
    pub go_env: BTreeMap<String, String>,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareOptimization<'a> {
    Named(&'a str),
    Custom(HardwareProfile),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizedHostPlatform {
    pub system: String,
    pub config: String,
    pub gcc: Option<GccMetadata>,
}

pub fn metadata() -> Result<Metadata> {
    serde_json::from_str(METADATA_JSON).map_err(Into::into)
}

pub fn target_for<'a>(metadata: &'a Metadata, system: &str) -> Result<&'a TargetDescriptor> {
    metadata
        .targets
        .get(system)
        .ok_or_else(|| Error::UnsupportedHostPlatform {
            system: system.to_owned(),
        })
}

pub fn nix_system_to_zig_target(metadata: &Metadata, system: &str) -> Result<String> {
    lookup_platform_mapping(
        "Zig target",
        &metadata.platform_mappings.zig_targets,
        system,
    )
}

pub fn nix_system_to_gnu_config(metadata: &Metadata, system: &str) -> Result<String> {
    lookup_platform_mapping(
        "GNU config",
        &metadata.platform_mappings.gnu_configs,
        system,
    )
}

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

pub fn hardware_optimization_name(
    metadata: &Metadata,
    hardware_optimization: Option<HardwareOptimization<'_>>,
) -> Result<Option<String>> {
    Ok(hardware_profile_for(metadata, hardware_optimization)?.map(|profile| profile.name))
}

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

pub fn os_of(system: &str) -> Option<&str> {
    system.rsplit('-').next()
}

pub fn is_linux(system: &str) -> bool {
    os_of(system) == Some("linux")
}

pub fn is_darwin(system: &str) -> bool {
    os_of(system) == Some("darwin")
}

pub fn is_windows(system: &str) -> bool {
    os_of(system) == Some("windows")
}

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
