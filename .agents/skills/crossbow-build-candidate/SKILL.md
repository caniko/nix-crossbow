---
name: crossbow-build-candidate
description: Evaluate whether a package should be optimized or cross-built with Crossbow, including cases where precompiled binaries exist. Use when deciding whether applyBuildOptimization, crossbowCrossPkgs, package-level cross overrides, or cache-miss optimizations are appropriate.
---

**Cross-repository work:** As soon as work is known to span more than one Git repository, invoke `$graphify` before further discovery, planning, or edits. Query a relevant existing graph first; build or update a merged graph if none exists, it is stale, or it does not cover every repository in scope. Reuse a current graph already produced for the same repository set.

# Crossbow Build Candidate

Use this skill to decide whether a package belongs in Crossbow optimization.
Be conservative: recommend inclusion only when all three gates pass.

## Required Gates

All must be true:

1. **Hot path**: the binary is on a service request path, boot/deploy path, frequent CLI path, or latency-sensitive path where optimization plausibly matters.
2. **Lightweight build**: the package builds quickly enough that local cross-building is cheaper than waiting on remote/native builders or carrying broad cache churn.
3. **Stable input**: the package is relatively stable, with low version churn and no frequent local override/hash refreshes.

If any gate fails, return `exclude`. If evidence is missing for any gate, return `needs evidence`.

## Evidence Workflow

Gather facts before deciding:

- Identify the package attr, consumer host/service, target platform, current derivation system, and whether a substituter already provides it.
- Inspect the package definition and service wiring. Prefer module-owned package defaults over host-level raw overrides so Crossbow policy stays centralized.
- Check build shape:
  ```bash
  drv=$(nix eval --raw <flake>#<package-or-config-package>.drvPath)
  nix derivation show "$drv" | jq '.derivations | to_entries[0].value | {system, env}'
  nix build --dry-run <flake>#<package-or-system>
  ```
- Check hot-path evidence from service config, deployment failures, profiling data, request path, boot path, or operator workflow.
- Check build cost with dry-run size, previous build logs, timing from a real build when safe, language/toolchain, dependency count, and closure impact.
- Check stability from git history, flake lock churn, package version history, vendor hash churn, and upstream release cadence.

Do not count “precompiled binaries exist” as an automatic exclusion. If binaries are hot, cheap to rebuild, and stable, Crossbow optimization may still be useful.

## Recommendation Rules

- Prefer package-level `applyBuildOptimization` over full-system strict cross.
- Prefer `crossbowCrossPkgs`-aware module defaults over per-host package overrides.
- Keep normal cached packages untouched when the binary is not hot or the build is expensive.
- Do not optimize broad dependency sets just because one leaf package is missing from cache.
- For Go packages, check whether `CGO_ENABLED=0`, `GOOS`, and `GOARCH` match the intended target.
- For Rust/C/C++ packages, verify the toolchain and hardware flags are applicable before recommending target tuning.

## Missing Evidence Rule

If foundational evidence is missing, stop and report:

- Missing artifact or source.
- Why it is required.
- Upstream producer to fix.
- Exact command or workflow to regenerate it.
- Validation command that proves it is fixed.

Do not fabricate timing, cache status, update cadence, or hot-path evidence.

## Output Contract

Return this shape:

```markdown
Decision: include | exclude | needs evidence

| Gate | Result | Evidence |
| --- | --- | --- |
| Hot path | pass/fail/missing | ... |
| Lightweight build | pass/fail/missing | ... |
| Stable input | pass/fail/missing | ... |

Suggested integration:
- ...

Validation:
- `...`
```

For `include`, name the narrowest integration point, such as a package option default, module override, or `crossbowCrossPkgs`-aware package selection.
For `exclude`, name the first failed gate and the evidence.
For `needs evidence`, name the missing evidence and the exact command or workflow to obtain it.
