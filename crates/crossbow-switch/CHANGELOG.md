# Changelog

## [0.1.0] - 2026-10-12

First registry release of the existing Crossbow runtime library and executable.

- Cache-shaped realization and prepared activation with substitution-only
  defaults and explicit remote-native realization policy.
- Typed executor, requirement, platform-metadata, and prepared-state APIs.
- Schema-3 plans with exact runtime paths, NAR hashes, direct references,
  dependency-first cross-substituter restoration, and retained GC roots.
- Compatibility with persisted schema-2 plan identities and execution.
- Shared maintained Serde/thiserror macro generation and warning-free native
  compiler checks, without changing runtime or persisted-state behavior.
- Portable Nix packaging for hosted workers, with managed compiler-cache
  wrapping available through the separate `crossbow-switch-cached` output.
