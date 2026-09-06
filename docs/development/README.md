# Development Guide

Entry point for VT Code contributor workflows.

## Getting Started

- [Development Setup](./DEVELOPMENT_SETUP.md) - Canonical local setup and quality loop.
- [Testing Guide](./testing.md) - Test commands, structure, and benchmark coverage, including the startup launch benchmark.
- [C++ Core Guidelines Adoption](./CPP_CORE_GUIDELINES_ADOPTION.md) - Policy for any C/C++ code introduced in this repository.
- [CI/CD](./ci-cd.md) - Pipeline behavior and verification stages.
- [Cross Compilation](./cross-compilation.md) - Multi-target build workflows.
- [Fuzzing](./fuzzing.md) - `cargo-fuzz` usage and parser hardening.

## Security and Execution

- [Process Hardening](./PROCESS_HARDENING.md) - Runtime hardening controls.
- [Execution Policy](./EXECUTION_POLICY.md) - Command policy model.
- [Command Security Model](./COMMAND_SECURITY_MODEL.md) - Command validation and threat model.
- [WebMCP bridge](./webmcp.md) - Authenticated browser editing, pairing, runtime adapters, and security boundaries.
- [Security Guide](../guides/security.md) - Process sandbox boundaries, MCP inheritance, and provider diagnostic redaction.
- [Runtime Guidance](./runtime-guidance.md) - Boundary between compiled universal behavior and dynamic project instructions.
- [vtcode Binary Gotchas](./vtcode-binary-gotchas.md) - Binary startup, runloop recovery, allocator, and request-assembly invariants.
- [AI Tool Surface Migration](./ai-tool-surface-migration.md) - Breaking-change notes for the Codex-style default tools.

## Performance and Reliability

- [Performance Guide](./performance.md) - Profiling and optimization workflow.
- [Rust Performance Principles](./rust-performance-principles.md) - Rust hot-path and I/O guidance.
- [Performance Hasher Policy](./performance-hasher-policy.md) - `rustc_hash` usage policy.
- [Async Performance Audit](./async-performance-audit.md) - Async architecture performance findings.
- [Session Event Persistence](./session-persistence.md) - Canonical session events, exporter boundaries, retention, and verification.
- [Configuration reset and live reload](./configuration-reload.md) - Shared reset service, watcher contract, runtime application, and verification.

## Model Management

- [Adding Models](./ADDING_MODELS.md) - Complete workflow for adding new LLM models.
- [Model Addition Checklist](./MODEL_ADDITION_CHECKLIST.md) - Step-by-step checklist for model additions.

## Maintenance Workflows

- [Asset Synchronization](./asset-synchronization.md) - Embedded asset maintenance.
- [AI Tool Surface Eval Report](./ai-tool-surface-eval-report.md) - Executable
  tool-surface cases, suite validation, and the baseline comparison format.
- [Changelog Generation](./CHANGELOG_GENERATION.md) - `git-cliff`-based changelog updates.
- [Desire Paths](./DESIRE_PATHS.md) - Known architecture pressure points.
- [TUI-Only Refactoring Notes](./TUI_ONLY_REFACTORING.md) - Historical refactor details.
- [Tool Summary Display](./tool-summary-display.md) - Compact and expanded tool transition summaries, configuration, and testing boundaries.
- [Preview Budget, Blocked Turns, and Replans](./preview-budget-blocked-replan.md) - Per-turn 32 KiB model-visible budget, `turn.blocked` handoff and resume, and mid-execution replan continuation.
- [First-party debt scan](../../scripts/first-party-debt-scan.sh) - Detect actionable debt markers while excluding generated and fixture content.

## Navigation

- [Documentation Hub](../README.md)
- [Docs Index](../INDEX.md)
- [Contributing](../CONTRIBUTING.md)
