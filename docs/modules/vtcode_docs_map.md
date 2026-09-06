# VT Code Documentation Map

This document serves as an index of all VT Code documentation. When users ask questions about VT Code itself (capabilities, features, configuration, etc.), this file provides the complete catalog of available documentation sources.

## Quick Reference

**Core Questions**: Can VT Code do X? | How does VT Code Y work? | What's VT Code's Z feature?

**Documentation Retrieval**: When users ask about VT Code capabilities, fetch relevant sections from the files listed below based on the topic area.

## Documentation Categories

### Configuration & Customization

- **File**: `docs/config/CONFIG_FIELD_REFERENCE.md`
  - **Content**: Config Field Reference
  - **User Questions**: "What can you tell me about Config Field Reference?"

- **File**: `docs/config/CONFIGURATION_PRECEDENCE.md`
  - **Content**: Configuration Precedence in VT Code
  - **Topics**: Resolution Order, Resetting a configuration layer, Live reload and invalid edits, Default Values, Validation
  - **User Questions**: "What can you tell me about Configuration Precedence in VT Code?", "How does Resolution Order work?", "How does Resetting a configuration layer work?"

- **File**: `docs/config/TOOLS_CONFIG.md`
  - **Content**: Tools Configuration
  - **Topics**: Blocked-call limits and blocked handoffs
  - **User Questions**: "What can you tell me about Tools Configuration?", "How does Blocked-call limits and blocked handoffs work?"

- **File**: `docs/config/config.md`
  - **Content**: VT Code Configuration
  - **Topics**: Quick navigation, Settings palette and reset, Live reload, Feature flags, Model selection
  - **User Questions**: "What can you tell me about VT Code Configuration?", "How does Quick navigation work?", "How does Settings palette and reset work?"

### Development & Testing

- **File**: `docs/development/asset-synchronization.md`
  - **Content**: **Asset Synchronization**
  - **Topics**: **Overview**, **Synchronized Assets**, **How It Works**, **Verification**, **Adding New Embedded Assets**
  - **User Questions**: "What can you tell me about **Asset Synchronization**?", "How does **Overview** work?", "How does **Synchronized Assets** work?"

- **File**: `docs/development/testing.md`
  - **Content**: **Testing Guide**
  - **Topics**: **Test Overview**, **Running Tests**, **Test Structure**, **Test Profiles & Groups**, **Test Categories**
  - **User Questions**: "What can you tell me about **Testing Guide**?", "How does **Test Overview** work?", "How does **Running Tests** work?"

- **File**: `docs/development/ai-tool-surface-eval-report.md`
  - **Content**: AI Tool Surface Eval Report
  - **Topics**: Scope, Baseline Data, Comparison Table, Result Format
  - **User Questions**: "What can you tell me about AI Tool Surface Eval Report?", "How does Scope work?", "How does Baseline Data work?"

- **File**: `docs/development/ai-tool-surface-migration.md`
  - **Content**: AI Tool Surface Migration
  - **Topics**: What Changed, Replacement Map, Short Examples, Advanced Profile, File Tool Finding
  - **User Questions**: "What can you tell me about AI Tool Surface Migration?", "How does What Changed work?", "How does Replacement Map work?"

- **File**: `docs/development/ADDING_MODELS.md`
  - **Content**: Adding New Models to VT Code
  - **Topics**: Overview, Quick Checklist, Detailed Steps, Verification, Template for Copy-Paste
  - **User Questions**: "What can you tell me about Adding New Models to VT Code?", "How does Overview work?", "How does Quick Checklist work?"

- **File**: `docs/development/ALLOCATOR_MEMORY.md`
  - **Content**: Allocator & Memory Behavior
  - **Topics**: The problem, Allocator selection, Allocation throughput trade-off (measured), Measuring it, Implementation
  - **User Questions**: "What can you tell me about Allocator & Memory Behavior?", "How does The problem work?", "How does Allocator selection work?"

- **File**: `docs/development/EXTENDED_THINKING.md`
  - **Content**: Anthropic Thinking in VT Code
  - **Topics**: Compact Runtime Matrix, Configuration, Adaptive Thinking Behavior, Budgeted Thinking Behavior, Feature Compatibility
  - **User Questions**: "What can you tell me about Anthropic Thinking in VT Code?", "How does Compact Runtime Matrix work?", "How does Configuration work?"

- **File**: `docs/development/CPP_CORE_GUIDELINES_ADOPTION.md`
  - **Content**: C++ Core Guidelines Adoption
  - **Topics**: Scope, Mandatory C++ Rules, Review and Enforcement
  - **User Questions**: "What can you tell me about C++ Core Guidelines Adoption?", "How does Scope work?", "How does Mandatory C++ Rules work?"

- **File**: `docs/development/ci-cd.md`
  - **Content**: CI/CD and Code Quality
  - **Topics**: GitHub Actions Workflows, Code Quality Tools, Local Development, Best Practices, CI/CD Configuration
  - **User Questions**: "What can you tell me about CI/CD and Code Quality?", "How does GitHub Actions Workflows work?", "How does Code Quality Tools work?"

- **File**: `docs/development/CHANGELOG_GENERATION.md`
  - **Content**: Changelog Generation with git-cliff
  - **Topics**: Configuration, Usage, Integration with Release Process, Commit Message Format, Excluded Commits
  - **User Questions**: "What can you tell me about Changelog Generation with git-cliff?", "How does Configuration work?", "How does Usage work?"

- **File**: `docs/development/configuration-reload.md`
  - **Content**: Configuration reset and live reload
  - **Topics**: Shared reset service, Runtime reload contract, Verification
  - **User Questions**: "What can you tell me about Configuration reset and live reload?", "How does Shared reset service work?", "How does Runtime reload contract work?"

- **File**: `docs/development/CGP_ARCHITECTURE.md`
  - **Content**: Context-Generic Programming (CGP) in VT Code
  - **Topics**: Overview, Core Concepts, Architecture, Runtime Contexts, Provider Traits
  - **User Questions**: "What can you tell me about Context-Generic Programming (CGP) in VT Code?", "How does Overview work?", "How does Core Concepts work?"

- **File**: `docs/development/cross-compilation.md`
  - **Content**: Cross-Compilation Configuration for VT Code
  - **Topics**: Overview, Configuration Details, Usage, Platform-Specific Notes, Integration with Release Process
  - **User Questions**: "What can you tell me about Cross-Compilation Configuration for VT Code?", "How does Overview work?", "How does Configuration Details work?"

- **File**: `docs/development/DESIRE_PATHS.md`
  - **Content**: Desire Paths in VT Code
  - **Topics**: Philosophy, Current Paved Paths, Desire Paths to Implement, How to Report Friction, Implementation Checklist
  - **User Questions**: "What can you tell me about Desire Paths in VT Code?", "How does Philosophy work?", "How does Current Paved Paths work?"

- **File**: `docs/development/README.md`
  - **Content**: Development Guide
  - **Topics**: Getting Started, Security and Execution, Performance and Reliability, Model Management, Maintenance Workflows
  - **User Questions**: "What can you tell me about Development Guide?", "How does Getting Started work?", "How does Security and Execution work?"

- **File**: `docs/development/DEVELOPMENT_SETUP.md`
  - **Content**: Development Setup
  - **Topics**: One-Time Setup, Credential identities, Daily Development Loop, Full Quality Gate, Common Commands
  - **User Questions**: "What can you tell me about Development Setup?", "How does One-Time Setup work?", "How does Credential identities work?"

- **File**: `docs/development/EXTENSION_BOUNDARIES.md`
  - **Content**: Extension Boundaries in VT Code
  - **Topics**: Why This Exists, Default Extension Order, What Counts As Internal, What Counts As External, Review Checklist
  - **User Questions**: "What can you tell me about Extension Boundaries in VT Code?", "How does Why This Exists work?", "How does Default Extension Order work?"

- **File**: `docs/development/fuzzing.md`
  - **Content**: Fuzzing Guide
  - **Topics**: Scope, Basic Commands, Corpus and Artifacts, Reproducing a Crash, Coverage (Optional)
  - **User Questions**: "What can you tell me about Fuzzing Guide?", "How does Scope work?", "How does Basic Commands work?"

- **File**: `docs/development/MODEL_ADDITION_CHECKLIST.md`
  - **Content**: Model Addition Checklist
  - **Topics**: Pre-Flight, Phase 1: Constants & Metadata (Database Layer), Phase 2: Model ID Resolution (Core Layer), Phase 3: Capabilities & Collections (Runtime Layer), Phase 4: Compilation & Verification
  - **User Questions**: "What can you tell me about Model Addition Checklist?", "How does Pre-Flight work?", "How does Phase 1: Constants & Metadata (Database Layer) work?"

- **File**: `docs/development/MODEL_ADDITION_WORKFLOW.md`
  - **Content**: Model Addition Workflow Summary
  - **Topics**: Why This Workflow?, The 10-Step Process, Automation Level: 60% (Guided, Not Fully Automated), Typical Workflow, File Dependency Graph
  - **User Questions**: "What can you tell me about Model Addition Workflow Summary?", "How does Why This Workflow? work?", "How does The 10-Step Process work?"

- **File**: `docs/development/performance-hasher-policy.md`
  - **Content**: Performance Hasher Policy
  - **Topics**: Default, When `rustc_hash` Is Allowed, When It Is Not Allowed, Migration Gate
  - **User Questions**: "What can you tell me about Performance Hasher Policy?", "How does Default work?", "How does When `rustc_hash` Is Allowed work?"

- **File**: `docs/development/performance.md`
  - **Content**: Performance Optimization
  - **Topics**: Goals, Performance & Simplicity Rules, Local Workflow, Standalone startup benchmark, Profiling Build
  - **User Questions**: "What can you tell me about Performance Optimization?", "How does Goals work?", "How does Performance & Simplicity Rules work?"

- **File**: `docs/development/preview-budget-blocked-replan.md`
  - **Content**: Preview Budget, Blocked Turns, and Replans
  - **User Questions**: "What can you tell me about Preview Budget, Blocked Turns, and Replans?"

- **File**: `docs/development/PROCESS_HARDENING.md`
  - **Content**: Process Hardening
  - **Topics**: Architecture, Security Measures, Implementation Details, Testing, Security Philosophy
  - **User Questions**: "What can you tell me about Process Hardening?", "How does Architecture work?", "How does Security Measures work?"

- **File**: `docs/development/runtime-guidance.md`
  - **Content**: Runtime Guidance and Project Instructions
  - **Topics**: User-facing progress contract, Continuity and long-running work
  - **User Questions**: "What can you tell me about Runtime Guidance and Project Instructions?", "How does User-facing progress contract work?", "How does Continuity and long-running work work?"

- **File**: `docs/development/rust-performance-principles.md`
  - **Content**: Rust-Specific Performance Principles for VT Code
  - **Topics**: Table of Contents, Core Insight: Rust Is Not Faster Than C/C++ — It Is *Safer While Being Equally Fast*, Destructive Move Semantics, Aliasing Guarantees (`noalias`), Immutable by Default & `const` Semantics
  - **User Questions**: "What can you tell me about Rust-Specific Performance Principles for VT Code?", "How does Table of Contents work?", "How does Core Insight: Rust Is Not Faster Than C/C++ — It Is *Safer While Being Equally Fast* work?"

- **File**: `docs/development/session-persistence.md`
  - **Content**: Session Event Persistence
  - **User Questions**: "What can you tell me about Session Event Persistence?"

- **File**: `docs/development/TUI_ONLY_REFACTORING.md`
  - **Content**: TUI-Only Tool Permission Refactoring
  - **Topics**: Overview, Problem Statement, Solution, Usage, Backward Compatibility
  - **User Questions**: "What can you tell me about TUI-Only Tool Permission Refactoring?", "How does Overview work?", "How does Problem Statement work?"

- **File**: `docs/development/grep-tool-guide.md`
  - **Content**: Text Search Guide
  - **Topics**: Overview, Architecture, Basic Usage, Flag Reference, Common Patterns
  - **User Questions**: "What can you tell me about Text Search Guide?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/development/tool-summary-display.md`
  - **Content**: Tool Summary Display
  - **Topics**: Model-visible tool output budget, Guardrails quick reference
  - **User Questions**: "What can you tell me about Tool Summary Display?", "How does Model-visible tool output budget work?", "How does Guardrails quick reference work?"

- **File**: `docs/development/async-performance-audit.md`
  - **Content**: VT Code Async Performance Audit
  - **Topics**: Audit Rubric, Findings (Prioritized), Implemented Batch (Runtime-Critical), Validation, Next Batch (Recommended)
  - **User Questions**: "What can you tell me about VT Code Async Performance Audit?", "How does Audit Rubric work?", "How does Findings (Prioritized) work?"

- **File**: `docs/development/COMMAND_SECURITY_MODEL.md`
  - **Content**: VT Code Command Security Model
  - **Topics**: Overview, Design Philosophy, Architecture, Safe Commands (Enabled by Default), Dangerous Commands (Always Denied)
  - **User Questions**: "What can you tell me about VT Code Command Security Model?", "How does Overview work?", "How does Design Philosophy work?"

- **File**: `docs/development/EXECUTION_POLICY.md`
  - **Content**: VT Code Execution Policy
  - **Topics**: Summary, Auto-Allowed Commands, Tool Policies, Key Safety Features, Dangerous Operations (Blocked)
  - **User Questions**: "What can you tell me about VT Code Execution Policy?", "How does Summary work?", "How does Auto-Allowed Commands work?"

- **File**: `docs/development/webmcp.md`
  - **Content**: WebMCP bridge development guide
  - **Topics**: Published deployments and exact origins, Boundaries, WebMCP API versus the VT Code bridge, Browser tool contracts and evals, Transport and pairing
  - **User Questions**: "What can you tell me about WebMCP bridge development guide?", "How does Published deployments and exact origins work?", "How does Boundaries work?"

- **File**: `docs/development/model-profiles.md`
  - **Content**: model-profiles.md
  - **User Questions**: "What can you tell me about model-profiles.md?"

- **File**: `docs/development/grep-quick-reference.md`
  - **Content**: rg Text Search Quick Reference Card
  - **Topics**: Essential Commands, Common Search Patterns, Smart Patterns by Language, Performance Tips, Output Example
  - **User Questions**: "What can you tell me about rg Text Search Quick Reference Card?", "How does Essential Commands work?", "How does Common Search Patterns work?"

- **File**: `docs/development/vtcode-binary-gotchas.md`
  - **Content**: vtcode Binary Gotchas
  - **Topics**: Startup, updates, and allocation, Tool-budget and recovery contracts, WebMCP bridge boundary, Request assembly and planning, Startup benchmark guidance
  - **User Questions**: "What can you tell me about vtcode Binary Gotchas?", "How does Startup, updates, and allocation work?", "How does Tool-budget and recovery contracts work?"

- **File**: `docs/development/vtcode-crate-audit.md`
  - **Content**: vtcode-* Crate Audit Report
  - **Topics**: Workspace Overview, MERGE RECOMMENDATIONS, RE-EXPORT CLEANUP (in vtcode-core), TEST DUPLICATES TO ELIMINATE, IMPLEMENTATION ORDER
  - **User Questions**: "What can you tell me about vtcode-* Crate Audit Report?", "How does Workspace Overview work?", "How does MERGE RECOMMENDATIONS work?"

### Editor Integrations

- **File**: `docs/ide/cursor-windsurf-setup.md`
  - **Content**: Cursor and Windsurf Setup Guide
  - **Topics**: Overview, Configuration, Features Available, Troubleshooting, Support
  - **User Questions**: "What can you tell me about Cursor and Windsurf Setup Guide?", "How does Overview work?", "How does Configuration work?"

- **File**: `docs/ide/editor-context-bridge.md`
  - **Content**: Editor Context Bridge
  - **Topics**: Contract, Canonical payload, Field notes, Integration checklist
  - **User Questions**: "What can you tell me about Editor Context Bridge?", "How does Contract work?", "How does Canonical payload work?"

- **File**: `docs/ide/downloads.md`
  - **Content**: VT Code Downloads
  - **Topics**: Available for Your IDE, What is VT Code?, Support and Documentation
  - **User Questions**: "What can you tell me about VT Code Downloads?", "How does Available for Your IDE work?", "How does What is VT Code? work?"

- **File**: `docs/ide/troubleshooting.md`
  - **Content**: VT Code Troubleshooting Guide
  - **Topics**: Extension Not Working, AI Provider Not Working, Slow Performance, Configuration Issues, VS Code-Compatible Editors
  - **User Questions**: "What can you tell me about VT Code Troubleshooting Guide?", "How does Extension Not Working work?", "How does AI Provider Not Working work?"

### Getting Started & Overview

- **File**: `docs/user-guide/agent-plugins.md`
  - **Content**: Agent Plugins
  - **Topics**: What a plugin gives you, Install a plugin, Manage plugins, Use plugin skills and MCP, Example: vtcode-plugins
  - **User Questions**: "What can you tell me about Agent Plugins?", "How does What a plugin gives you work?", "How does Install a plugin work?"

- **File**: `docs/concurrency-improvements-report.md`
  - **Content**: Concurrency Improvements Report
  - **Topics**: Executive Summary, Changes Made, Bug Summary Table, False Positives Eliminated, Test Results
  - **User Questions**: "What can you tell me about Concurrency Improvements Report?", "How does Executive Summary work?", "How does Changes Made work?"

- **File**: `docs/user-guide/exec-mode.md`
  - **Content**: Exec Mode Automation
  - **Topics**: Launching exec mode, Dry-run mode, Continuation and verification, Structured event stream, Resuming sessions
  - **User Questions**: "What can you tell me about Exec Mode Automation?", "How does Launching exec mode work?", "How does Dry-run mode work?"

- **File**: `docs/user-guide/getting-started.md`
  - **Content**: Getting Started with VT Code
  - **Topics**: What Makes VT Code Special, Enhanced Terminal Interface, Configuration, Usage Examples, Understanding the Agents
  - **User Questions**: "What can you tell me about Getting Started with VT Code?", "How does What Makes VT Code Special work?", "How does Enhanced Terminal Interface work?"

- **File**: `docs/loop-engineering.md`
  - **Content**: Loop Engineering in vtcode
  - **Topics**: The Five Primitives, Reconcile Flow, Configuration, Feature Flags
  - **User Questions**: "What can you tell me about Loop Engineering in vtcode?", "How does The Five Primitives work?", "How does Reconcile Flow work?"

- **File**: `docs/user-guide/scheduled-tasks.md`
  - **Content**: Scheduled Tasks
  - **Topics**: Session-scoped scheduling, Durable scheduling, Config and disable flags, Security model
  - **User Questions**: "What can you tell me about Scheduled Tasks?", "How does Session-scoped scheduling work?", "How does Durable scheduling work?"

- **File**: `docs/SECURITY.md`
  - **Content**: Security Policy
  - **Topics**: Reporting a Vulnerability, Security Best Practices for Users, Supported Versions, Security Features, CI/CD Security Controls
  - **User Questions**: "What can you tell me about Security Policy?", "How does Reporting a Vulnerability work?", "How does Security Best Practices for Users work?"

- **File**: `docs/signal_handling.md`
  - **Content**: Signal Handling Architecture
  - **Topics**: Priority Guarantees, Signal Handling Components, Interaction Loop Exit Check, Process Group Management, Emergency Terminal Cleanup
  - **User Questions**: "What can you tell me about Signal Handling Architecture?", "How does Priority Guarantees work?", "How does Signal Handling Components work?"

- **File**: `docs/user-guide/subagents.md`
  - **Content**: Subagents
  - **Topics**: Built-in primary agents, Built-in subagents, Summary, Facts, Touched Files
  - **User Questions**: "What can you tell me about Subagents?", "How does Built-in primary agents work?", "How does Built-in subagents work?"

- **File**: `docs/user-guide/tree-sitter-integration.md`
  - **Content**: Tree-sitter Integration
  - **Topics**: Overview, Shell Safety Parsing, Architecture, Technical Details, Configuration
  - **User Questions**: "What can you tell me about Tree-sitter Integration?", "How does Overview work?", "How does Shell Safety Parsing work?"

- **File**: `docs/ARCHITECTURE.md`
  - **Content**: VT Code Architecture Guide
  - **Topics**: Overview, Model + Harness, Core Architecture, Tool Implementations, Design Principles
  - **User Questions**: "What can you tell me about VT Code Architecture Guide?", "How does Overview work?", "How does Model + Harness work?"

- **File**: `docs/COMPATIBILITY.md`
  - **Content**: VT Code Platform Compatibility Matrix
  - **Topics**: Quick Reference, Minimum Supported Rust Version (MSRV), Platform-Specific Features, LLM Provider Compatibility, Feature Flags Matrix
  - **User Questions**: "What can you tell me about VT Code Platform Compatibility Matrix?", "How does Quick Reference work?", "How does Minimum Supported Rust Version (MSRV) work?"

- **File**: `docs/user-guide/webmcp.md`
  - **Content**: WebMCP Browser Bridge User Guide
  - **Topics**: Two integration paths, Evaluate the browser tool surface, Run the WebMCP app without a bridge, Connect to a real workspace, Use the read-only remote MCP surface
  - **User Questions**: "What can you tell me about WebMCP Browser Bridge User Guide?", "How does Two integration paths work?", "How does Evaluate the browser tool surface work?"

- **File**: `docs/user-guide/webmcp-demo.md`
  - **Content**: WebMCP browser bridge guide (legacy link)
  - **User Questions**: "What can you tell me about WebMCP browser bridge guide (legacy link)?"

### Integrations & Tooling

- **File**: `docs/guides/acp-registry-release-checklist.md`
  - **Content**: ACP Registry Release Checklist
  - **Topics**: Preconditions, 1) Build and publish a macOS asset (local), 2) Build Linux assets (GitHub Actions), 3) (Optional) Build Windows asset, 4) Update ACP registry entry
  - **User Questions**: "What can you tell me about ACP Registry Release Checklist?", "How does Preconditions work?", "How does 1) Build and publish a macOS asset (local) work?"

- **File**: `docs/guides/agent-capability-composition.md`
  - **Content**: Agent Capability as a Composed System
  - **Topics**: The seven components, The three environmental constraints, The progress invariant, How the pieces reinforce each other
  - **User Questions**: "What can you tell me about Agent Capability as a Composed System?", "How does The seven components work?", "How does The three environmental constraints work?"

- **File**: `docs/guides/INIT_COMMAND_GUIDE.md`
  - **Content**: Agent Initialization Guide
  - **Topics**: Overview, Key Features, Usage, Generated Content Structure, Relationship to Rules and Memory
  - **User Questions**: "What can you tell me about Agent Initialization Guide?", "How does Overview work?", "How does Key Features work?"

- **File**: `docs/guides/agent-loop-contract.md`
  - **Content**: Agent Loop Contract
  - **Topics**: Message and Event Mapping, Terminal Thread Result, Canonical event persistence, Compaction Boundary, Fresh Plan Execution Context
  - **User Questions**: "What can you tell me about Agent Loop Contract?", "How does Message and Event Mapping work?", "How does Terminal Thread Result work?"

- **File**: `docs/guides/agent-plugins.md`
  - **Content**: Agent Plugins
  - **Topics**: Directory layout, Manifest, Discovery roots, Example usage, Skills
  - **User Questions**: "What can you tell me about Agent Plugins?", "How does Directory layout work?", "How does Manifest work?"

- **File**: `docs/guides/cargo-unmaintained.md`
  - **Content**: Cargo-Unmaintained
  - **Topics**: Overview, Usage, Configuration, Exit Codes, Common Options
  - **User Questions**: "What can you tell me about Cargo-Unmaintained?", "How does Overview work?", "How does Usage work?"

- **File**: `docs/guides/status-line.md`
  - **Content**: Configuring the inline status line
  - **Topics**: Available modes, Command payload structure, Example script
  - **User Questions**: "What can you tell me about Configuring the inline status line?", "How does Available modes work?", "How does Command payload structure work?"

- **File**: `docs/guides/full-automation.md`
  - **Content**: Full Automation
  - **Topics**: Activation Checklist, Runtime Behaviour, Customising The Allow-List, Propose/Verify Sub-agent, Orchestrated Harness
  - **User Questions**: "What can you tell me about Full Automation?", "How does Activation Checklist work?", "How does Runtime Behaviour work?"

- **File**: `docs/guides/permissions.md`
  - **Content**: Granular Permissions
  - **Topics**: Agent Permission Schema, Global Rules, Classifier Review Settings, Rule Grammar, Path Matching
  - **User Questions**: "What can you tell me about Granular Permissions?", "How does Agent Permission Schema work?", "How does Global Rules work?"

- **File**: `docs/guides/memory-management.md`
  - **Content**: Guidance and Persistent Memory for VT Code
  - **Topics**: Authored Guidance, Persistent Memory, Interactive Controls, `/init` and Scaffolding, Recommended Practices
  - **User Questions**: "What can you tell me about Guidance and Persistent Memory for VT Code?", "How does Authored Guidance work?", "How does Persistent Memory work?"

- **File**: `docs/guides/inline-ui.md`
  - **Content**: Inline UI session architecture
  - **Topics**: Core components, Rendering pipeline, Status line customization
  - **User Questions**: "What can you tell me about Inline UI session architecture?", "How does Core components work?", "How does Rendering pipeline work?"

- **File**: `docs/guides/lifecycle-hooks.md`
  - **Content**: Lifecycle Hooks
  - **Topics**: Configuration Overview, Matchers, Hook Execution Model, Event Reference, Interpreting Hook Results
  - **User Questions**: "What can you tell me about Lifecycle Hooks?", "How does Configuration Overview work?", "How does Matchers work?"

- **File**: `docs/guides/local-models.md`
  - **Content**: Local Models: Ollama, LM Studio & llama.cpp
  - **Topics**: Local vs Remote, Hardware & model sizing, Getting reliable results, Known limitations, See also
  - **User Questions**: "What can you tell me about Local Models: Ollama, LM Studio & llama.cpp?", "How does Local vs Remote work?", "How does Hardware & model sizing work?"

- **File**: `docs/guides/mcp-integration.md`
  - **Content**: MCP Integration Guide
  - **Topics**: MCP Specification Map, Configuring MCP Providers, Security and validation, Allowlist Behaviour, Testing the Integration
  - **User Questions**: "What can you tell me about MCP Integration Guide?", "How does MCP Specification Map work?", "How does Configuring MCP Providers work?"

- **File**: `docs/mcp/MCP_INTEGRATION_GUIDE.md`
  - **Content**: MCP Integration Guide for VT Code
  - **Topics**: Overview, Architecture, Configuration, Transport Types, Security
  - **User Questions**: "What can you tell me about MCP Integration Guide for VT Code?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/guides/minimax-integration.md`
  - **Content**: MiniMax Integration Guide
  - **Topics**: Overview, Configuration Options, Supported Features, Limitations, Example Usage
  - **User Questions**: "What can you tell me about MiniMax Integration Guide?", "How does Overview work?", "How does Configuration Options work?"

- **File**: `docs/guides/oauth-authentication.md`
  - **Content**: OAuth Authentication Guide
  - **Topics**: Overview, Supported Providers, Security Model, CLI Usage, Token Lifecycle
  - **User Questions**: "What can you tell me about OAuth Authentication Guide?", "How does Overview work?", "How does Supported Providers work?"

- **File**: `docs/guides/pty-integration-testing.md`
  - **Content**: PTY Integration Testing Guide
  - **Topics**: Automated Verification, Manual TUI Walkthrough, Troubleshooting
  - **User Questions**: "What can you tell me about PTY Integration Testing Guide?", "How does Automated Verification work?", "How does Manual TUI Walkthrough work?"

- **File**: `docs/guides/planning-workflow.md`
  - **Content**: Planning Workflow
  - **Topics**: Overview, Bounded blocked-call recovery, Usage, Plan Output Format, Summary
  - **User Questions**: "What can you tell me about Planning Workflow?", "How does Overview work?", "How does Bounded blocked-call recovery work?"

- **File**: `docs/guides/responses-api-reasoning.md`
  - **Content**: Responses API & Reasoning Models
  - **Topics**: Key Concepts, VT Code configuration guidance, Example workflow, Taking it further
  - **User Questions**: "What can you tell me about Responses API & Reasoning Models?", "How does Key Concepts work?", "How does VT Code configuration guidance work?"

- **File**: `docs/guides/security.md`
  - **Content**: Security Guide
  - **Topics**: Overview, Security Architecture, Threat Model, Configuration, Best Practices
  - **User Questions**: "What can you tell me about Security Guide?", "How does Overview work?", "How does Security Architecture work?"

- **File**: `docs/guides/tui-library.md`
  - **Content**: TUI Library Guide
  - **Topics**: What It Provides, Current Architecture, Usage, Host Traits
  - **User Questions**: "What can you tell me about TUI Library Guide?", "How does What It Provides work?", "How does Current Architecture work?"

- **File**: `docs/guides/COLOR_GUIDELINES.md`
  - **Content**: Terminal Color Guidelines
  - **Topics**: Standards Implemented, Configuration Reference, Light/Dark Mode Detection, Bold-is-Bright Compatibility, Available Themes
  - **User Questions**: "What can you tell me about Terminal Color Guidelines?", "How does Standards Implemented work?", "How does Configuration Reference work?"

- **File**: `docs/guides/terminal-rendering-best-practices.md`
  - **Content**: Terminal Rendering Best Practices for VT Code
  - **Topics**: Core Principle: Single Draw Per Frame, Viewport Management, Layout Computation, Rendering Widgets, Reflow and Text Wrapping
  - **User Questions**: "What can you tell me about Terminal Rendering Best Practices for VT Code?", "How does Core Principle: Single Draw Per Frame work?", "How does Viewport Management work?"

- **File**: `docs/guides/tool_registry.md`
  - **Content**: Tool Registry Guide
  - **Topics**: Registry architecture, Shell prompt profiles, Adding a new tool, Safety guidelines, Testing checklist
  - **User Questions**: "What can you tell me about Tool Registry Guide?", "How does Registry architecture work?", "How does Shell prompt profiles work?"

- **File**: `docs/guides/async-architecture.md`
  - **Content**: VT Code Async Architecture Guide
  - **Topics**: When Should VT Code Use Async?, Architecture: Async vs. Synchronous Paths, Key Async Patterns in VT Code, Anti-Patterns to Avoid, Integration with Event Loop
  - **User Questions**: "What can you tell me about VT Code Async Architecture Guide?", "How does When Should VT Code Use Async? work?", "How does Architecture: Async vs. Synchronous Paths work?"

- **File**: `docs/guides/code-organization-patterns.md`
  - **Content**: VT Code Code Organization Patterns
  - **Topics**: Scope Rules, Shared Ownership Rules, FFI and Process-Handle Field Lifetimes, Background Task Lifecycle, Channel Boundaries
  - **User Questions**: "What can you tell me about VT Code Code Organization Patterns?", "How does Scope Rules work?", "How does Shared Ownership Rules work?"

- **File**: `docs/guides/hooks-guide.md`
  - **Content**: VT Code Hooks System Documentation
  - **Topics**: Overview, Configuration, Hook Events, Hook Matching, Hook Scripts
  - **User Questions**: "What can you tell me about VT Code Hooks System Documentation?", "How does Overview work?", "How does Configuration work?"

- **File**: `docs/guides/output_styles.md`
  - **Content**: VT Code Output Styles Feature
  - **Topics**: Overview, How It Works, Configuration, Available Styles, Creating Custom Styles
  - **User Questions**: "What can you tell me about VT Code Output Styles Feature?", "How does Overview work?", "How does How It Works work?"

- **File**: `docs/guides/tui-event-handling.md`
  - **Content**: VT Code TUI Event Handling Guide
  - **Topics**: Overview, Key Implementation Details, Event Types, Configuration, Integration with VT Code
  - **User Questions**: "What can you tell me about VT Code TUI Event Handling Guide?", "How does Overview work?", "How does Key Implementation Details work?"

- **File**: `docs/guides/terminal-optimization.md`
  - **Content**: VT Code Terminal Optimization Guide
  - **Topics**: Table of Contents, Theme and Appearance, Line Break Options, Paste Handling, Notification Setup
  - **User Questions**: "What can you tell me about VT Code Terminal Optimization Guide?", "How does Table of Contents work?", "How does Theme and Appearance work?"

- **File**: `docs/guides/UPDATE_SYSTEM.md`
  - **Content**: VT Code Update System Guide
  - **Topics**: Overview, Release Channels, Version Pinning, Configuration File, CLI Reference
  - **User Questions**: "What can you tell me about VT Code Update System Guide?", "How does Overview work?", "How does Release Channels work?"

- **File**: `docs/guides/user-data-directories.md`
  - **Content**: VT Code user data directories
  - **Topics**: Find the active paths, Directory categories, Environment variables, Configuration precedence, Migration from `~/.vtcode`
  - **User Questions**: "What can you tell me about VT Code user data directories?", "How does Find the active paths work?", "How does Directory categories work?"

- **File**: `docs/guides/zed-acp.md`
  - **Content**: Zed Agent Client Protocol Integration
  - **Topics**: Setup overview, Build VT Code, Configure VT Code for ACP, Manual smoke test, Register VT Code in Zed
  - **User Questions**: "What can you tell me about Zed Agent Client Protocol Integration?", "How does Setup overview work?", "How does Build VT Code work?"

- **File**: `docs/guides/macos-alt-shortcut-troubleshooting.md`
  - **Content**: macOS Alt Shortcut Troubleshooting Guide
  - **Topics**: Overview, Common Symptoms, Root Causes, Solutions, Platform-Specific Guidance
  - **User Questions**: "What can you tell me about macOS Alt Shortcut Troubleshooting Guide?", "How does Overview work?", "How does Common Symptoms work?"

### LLM Providers & Models

- **File**: `docs/providers/atlascloud.md`
  - **Content**: Atlas Cloud Integration Guide
  - **Topics**: Quickstart, Why This Works, Model Selection Notes, Validated 50-model pool, CLI Examples
  - **User Questions**: "What can you tell me about Atlas Cloud Integration Guide?", "How does Quickstart work?", "How does Why This Works work?"

- **File**: `docs/providers/copilot.md`
  - **Content**: GitHub Copilot Managed Auth
  - **Topics**: What You Need, If `copilot` Is Missing, If `gh` Is Missing, Troubleshooting
  - **User Questions**: "What can you tell me about GitHub Copilot Managed Auth?", "How does What You Need work?", "How does If `copilot` Is Missing work?"

- **File**: `docs/providers/lmstudio-quick-reference.md`
  - **Content**: LM Studio Client Quick Reference
  - **Topics**: Module, Common Tasks, API Reference, Error Handling, CLI Tool Discovery
  - **User Questions**: "What can you tell me about LM Studio Client Quick Reference?", "How does Module work?", "How does Common Tasks work?"

- **File**: `docs/providers/LMSTUDIO_INTEGRATION.md`
  - **Content**: LM Studio Integration for VT Code
  - **Topics**: Overview, Architecture, API Reference, Configuration, Error Handling
  - **User Questions**: "What can you tell me about LM Studio Integration for VT Code?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/providers/lmstudio.md`
  - **Content**: LM Studio Provider Guide
  - **Topics**: Configuration, API Endpoints, Using Custom LM Studio Models, Tool Calling, Structured Output, and Streaming, Troubleshooting
  - **User Questions**: "What can you tell me about LM Studio Provider Guide?", "How does Configuration work?", "How does API Endpoints work?"

- **File**: `docs/providers/local-servers.md`
  - **Content**: Local Inference Servers
  - **Topics**: Supported Providers, Running Ollama, Running LM Studio, Running llama.cpp, Command Reference
  - **User Questions**: "What can you tell me about Local Inference Servers?", "How does Supported Providers work?", "How does Running Ollama work?"

- **File**: `docs/providers/merge-gateway.md`
  - **Content**: Merge Gateway Integration
  - **Topics**: Setup, Curated models, Configuration examples, Responses and catalog behavior, Troubleshooting
  - **User Questions**: "What can you tell me about Merge Gateway Integration?", "How does Setup work?", "How does Curated models work?"

- **File**: `docs/providers/merge-gateway-quick-reference.md`
  - **Content**: Merge Gateway Quick Reference
  - **Topics**: Minimal setup
  - **User Questions**: "What can you tell me about Merge Gateway Quick Reference?", "How does Minimal setup work?"

- **File**: `docs/providers/meta.md`
  - **Content**: Meta AI Integration Guide
  - **Topics**: Quickstart, Curated models, Troubleshooting
  - **User Questions**: "What can you tell me about Meta AI Integration Guide?", "How does Quickstart work?", "How does Curated models work?"

- **File**: `docs/providers/meta-quick-reference.md`
  - **Content**: Meta AI Quick Reference
  - **User Questions**: "What can you tell me about Meta AI Quick Reference?"

- **File**: `docs/providers/nvidia.md`
  - **Content**: NVIDIA NIM Provider Guide
  - **Topics**: Setup, Curated models, Reasoning and tools, References
  - **User Questions**: "What can you tell me about NVIDIA NIM Provider Guide?", "How does Setup work?", "How does Curated models work?"

- **File**: `docs/providers/nvidia-quick-reference.md`
  - **Content**: NVIDIA NIM Quick Reference
  - **User Questions**: "What can you tell me about NVIDIA NIM Quick Reference?"

- **File**: `docs/providers/OLLAMA_INDEX.md`
  - **Content**: Ollama Integration Documentation Index
  - **Topics**: Quick Links, Modules Overview, Data Flow, Integration Roadmap, Common Patterns
  - **User Questions**: "What can you tell me about Ollama Integration Documentation Index?", "How does Quick Links work?", "How does Modules Overview work?"

- **File**: `docs/providers/ollama-quick-reference.md`
  - **Content**: Ollama Module Quick Reference
  - **Topics**: Modules, Common Tasks, Type Hierarchy, API Methods, Error Handling
  - **User Questions**: "What can you tell me about Ollama Module Quick Reference?", "How does Modules work?", "How does Common Tasks work?"

- **File**: `docs/providers/ollama.md`
  - **Content**: Ollama Provider Guide
  - **Topics**: Configuration, Using Custom Ollama Models, OpenAI OSS Models Support, Laguna XS.2 Model, Tool calling and web search integration
  - **User Questions**: "What can you tell me about Ollama Provider Guide?", "How does Configuration work?", "How does Using Custom Ollama Models work?"

- **File**: `docs/providers/omniroute.md`
  - **Content**: OmniRoute Integration Guide
  - **Topics**: Configuration, Model Selection, Protocol Behavior, Remote and Container Setups, Troubleshooting
  - **User Questions**: "What can you tell me about OmniRoute Integration Guide?", "How does Configuration work?", "How does Model Selection work?"

- **File**: `docs/providers/openrouter.md`
  - **Content**: OpenRouter Integration Guide
  - **Topics**: Quickstart, Meta Muse models, Persisting configuration, Runtime behaviour, Troubleshooting
  - **User Questions**: "What can you tell me about OpenRouter Integration Guide?", "How does Quickstart work?", "How does Meta Muse models work?"

- **File**: `docs/providers/PROVIDER_GUIDES.md`
  - **Content**: Provider Guides
  - **Topics**: Provider whitelisting, Custom providers, Collapsed tool-result disclosure, Google Gemini, OpenAI
  - **User Questions**: "What can you tell me about Provider Guides?", "How does Provider whitelisting work?", "How does Custom providers work?"

- **File**: `docs/providers/vercel-ai-gateway.md`
  - **Content**: Vercel AI Gateway Integration
  - **Topics**: Setup, Curated models, Persisting configuration, Shared model IDs and provider precedence, Runtime behaviour
  - **User Questions**: "What can you tell me about Vercel AI Gateway Integration?", "How does Setup work?", "How does Curated models work?"

- **File**: `docs/providers/vercel-ai-gateway-quick-reference.md`
  - **Content**: Vercel AI Gateway Quick Reference
  - **User Questions**: "What can you tell me about Vercel AI Gateway Quick Reference?"

- **File**: `docs/providers/zai.md`
  - **Content**: Z.AI Provider Guide
  - **Topics**: Quickstart, Model API — GLM-5.3 Flash, Curated models, Vision-driven workflows (Flash), GLM Coding Plan
  - **User Questions**: "What can you tell me about Z.AI Provider Guide?", "How does Quickstart work?", "How does Model API — GLM-5.3 Flash work?"

- **File**: `docs/providers/zai-quick-reference.md`
  - **Content**: Z.AI Quick Reference
  - **User Questions**: "What can you tell me about Z.AI Quick Reference?"

- **File**: `docs/providers/llamacpp.md`
  - **Content**: llama.cpp Provider Guide
  - **Topics**: What VT Code manages, Manual server mode, Configuration, Notes
  - **User Questions**: "What can you tell me about llama.cpp Provider Guide?", "How does What VT Code manages work?", "How does Manual server mode work?"

- **File**: `docs/models.json`
  - **Content**: models.json Metadata
  - **Topics**: Model Specifications, Capabilities, Context Limits
  - **User Questions**: "What can you tell me about models.json Metadata?", "How does Model Specifications work?", "How does Capabilities work?"

### Modules & Implementation

- **File**: `docs/modules/println_tui_audit.md`
  - **Content**: Audit: Raw println!/print! calls that could leak into the TUI
  - **Topics**: Summary, Current Status: All Known Issues Fixed, Remaining Low-Risk Items, Safe I/O Patterns (Verified), Recommendations
  - **User Questions**: "What can you tell me about Audit: Raw println!/print! calls that could leak into the TUI?", "How does Summary work?", "How does Current Status: All Known Issues Fixed work?"

- **File**: `docs/modules/vtcode_config_migration.md`
  - **Content**: Migrating to the `vtcode-config` Crate
  - **Topics**: Migration Checklist, Rolling Adoption Strategy, Additional Resources
  - **User Questions**: "What can you tell me about Migrating to the `vtcode-config` Crate?", "How does Migration Checklist work?", "How does Rolling Adoption Strategy work?"

- **File**: `docs/modules/vtcode_commons_reference.md`
  - **Content**: Reference Implementations for `vtcode-commons`
  - **Topics**: Workspace Paths, Telemetry, Error Reporting, Putting It Together
  - **User Questions**: "What can you tell me about Reference Implementations for `vtcode-commons`?", "How does Workspace Paths work?", "How does Telemetry work?"

- **File**: `docs/modules/vtcode_bash_runner.md`
  - **Content**: `vtcode-bash-runner`
  - **Topics**: Core Concepts, Shell Selection and Portability, Policy Hooks, Dry-Run and Testing Example, Pure-Rust Execution
  - **User Questions**: "What can you tell me about `vtcode-bash-runner`?", "How does Core Concepts work?", "How does Shell Selection and Portability work?"

- **File**: `docs/modules/vtcode_exec_events.md`
  - **Content**: `vtcode-exec-events`
  - **Topics**: Event taxonomy, Versioning and compatibility, Feature flags, Integrating with VT Code runtimes, Examples
  - **User Questions**: "What can you tell me about `vtcode-exec-events`?", "How does Event taxonomy work?", "How does Versioning and compatibility work?"

- **File**: `docs/modules/vtcode_llm_environment.md`
  - **Content**: `vtcode-llm` Environment Configuration Guide
  - **Topics**: Provider environment variables, Loading keys with `ProviderConfig`, Wiring workspace paths and telemetry, Using the optional mock client
  - **User Questions**: "What can you tell me about `vtcode-llm` Environment Configuration Guide?", "How does Provider environment variables work?", "How does Loading keys with `ProviderConfig` work?"

- **File**: `docs/modules/vtcode_markdown_store.md`
  - **Content**: `vtcode-markdown-store`
  - **Topics**: Storage building blocks, Feature flags, Usage examples, Concurrency guarantees
  - **User Questions**: "What can you tell me about `vtcode-markdown-store`?", "How does Storage building blocks work?", "How does Feature flags work?"

- **File**: `docs/modules/vtcode_a2a.md`
  - **Content**: vtcode-a2a
  - **Topics**: Overview, Module Groups, Key Concepts, Feature Flags, Rules
  - **User Questions**: "What can you tell me about vtcode-a2a?", "How does Overview work?", "How does Module Groups work?"

- **File**: `docs/modules/vtcode_agent_plugins.md`
  - **Content**: vtcode-agent-plugins
  - **Topics**: Overview, Module Groups, Key Components, Runtime Integration, See Also
  - **User Questions**: "What can you tell me about vtcode-agent-plugins?", "How does Overview work?", "How does Module Groups work?"

- **File**: `docs/modules/vtcode_llm.md`
  - **Content**: vtcode-llm
  - **Topics**: Overview, Key Modules, Supported Providers, Architecture Notes, Dependencies
  - **User Questions**: "What can you tell me about vtcode-llm?", "How does Overview work?", "How does Key Modules work?"

- **File**: `docs/modules/vtcode_mcp.md`
  - **Content**: vtcode-mcp
  - **Topics**: Overview, Module Groups, Key Components, Architecture Notes, Configuration
  - **User Questions**: "What can you tell me about vtcode-mcp?", "How does Overview work?", "How does Module Groups work?"

- **File**: `docs/modules/vtcode_safety.md`
  - **Content**: vtcode-safety
  - **Topics**: Overview, Module Groups, Command Safety, Execution Policy, Sandboxing
  - **User Questions**: "What can you tell me about vtcode-safety?", "How does Overview work?", "How does Module Groups work?"

- **File**: `docs/modules/vtcode_skills.md`
  - **Content**: vtcode-skills
  - **Topics**: Overview, Key Modules, Skill Lifecycle, Architecture Notes, Dependencies
  - **User Questions**: "What can you tell me about vtcode-skills?", "How does Overview work?", "How does Key Modules work?"

- **File**: `docs/modules/vtcode_tools_policy.md`
  - **Content**: vtcode-tools Policy Customization Guide (Historical)
  - **Topics**: Custom storage location, Construct a `ToolPolicyManager` with your path
  - **User Questions**: "What can you tell me about vtcode-tools Policy Customization Guide (Historical)?", "How does Custom storage location work?", "How does Construct a `ToolPolicyManager` with your path work?"

- **File**: `docs/modules/vtcode_ui.md`
  - **Content**: vtcode-ui
  - **Topics**: Overview, Architecture, Key Components, Usage, Notes
  - **User Questions**: "What can you tell me about vtcode-ui?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/modules/vtcode_indexer.md`
  - **Content**: vtcode_indexer.md
  - **Topics**: Core concepts, Customizing persistence, Tailoring traversal, End-to-end example
  - **User Questions**: "What can you tell me about vtcode_indexer.md?", "How does Core concepts work?", "How does Customizing persistence work?"

### Other

- **File**: `docs/a2a/a2a-protocol.md`
  - **Content**: A2A Protocol Support for VT Code
  - **Topics**: Overview, Architecture, Core Types, Task Manager API, Server API (HTTP Endpoints)
  - **User Questions**: "What can you tell me about A2A Protocol Support for VT Code?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/acp/ACP_INTEGRATION.md`
  - **Content**: ACP (Agent Communication Protocol) Integration Guide
  - **Topics**: Overview, Architecture, Module Structure, Usage Examples, Initialization
  - **User Questions**: "What can you tell me about ACP (Agent Communication Protocol) Integration Guide?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/acp/ACP_QUICK_REFERENCE.md`
  - **Content**: ACP Quick Reference
  - **Topics**: Initialize ACP Client, Register Remote Agent, Discover Agents, Call Remote Agent (Sync), Call Remote Agent (Async)
  - **User Questions**: "What can you tell me about ACP Quick Reference?", "How does Initialize ACP Client work?", "How does Register Remote Agent work?"

- **File**: `docs/ansi/ANSI_STRIPPING_GUIDE.md`
  - **Content**: ANSI Code Stripping in Tool Output
  - **Topics**: Overview, Configuration, What Gets Stripped, Examples, Affected Tools
  - **User Questions**: "What can you tell me about ANSI Code Stripping in Tool Output?", "How does Overview work?", "How does Configuration work?"

- **File**: `docs/reference/README.md`
  - **Content**: ANSI Escape Sequences Documentation Index
  - **Topics**: WebMCP, Overview, Documents, Quick Navigation, Code Examples
  - **User Questions**: "What can you tell me about ANSI Escape Sequences Documentation Index?", "How does WebMCP work?", "How does Overview work?"

- **File**: `docs/reference/ansi-escape-sequences.md`
  - **Content**: ANSI Escape Sequences Reference
  - **Topics**: Sequences, General ASCII Codes, Cursor Controls, Erase Functions, Colors / Graphics Mode
  - **User Questions**: "What can you tell me about ANSI Escape Sequences Reference?", "How does Sequences work?", "How does General ASCII Codes work?"

- **File**: `docs/reference/ansi-in-vtcode.md`
  - **Content**: ANSI Escape Sequences in VT Code
  - **Topics**: Overview, Key Modules, ANSI Sequences Used in VT Code, PTY Output Processing, TUI Rendering
  - **User Questions**: "What can you tell me about ANSI Escape Sequences in VT Code?", "How does Overview work?", "How does Key Modules work?"

- **File**: `docs/reference/ansi-quick-reference.md`
  - **Content**: ANSI Quick Reference for VT Code Development
  - **Topics**: Most Common Sequences, VT Code Usage Examples, Regex Patterns, Testing Helpers, Common Mistakes
  - **User Questions**: "What can you tell me about ANSI Quick Reference for VT Code Development?", "How does Most Common Sequences work?", "How does VT Code Usage Examples work?"

- **File**: `docs/audits/agent-harness-duplication-report.md`
  - **Content**: Agent / Harness Duplication & Optimization Report
  - **Topics**: Architectural root cause, Findings, Revised status after line-by-line verification, Open questions for maintainers (behavior-changing — not auto-applied), Changelog
  - **User Questions**: "What can you tell me about Agent / Harness Duplication & Optimization Report?", "How does Architectural root cause work?", "How does Findings work?"

- **File**: `docs/harness/AGENT_LEGIBILITY_GUIDE.md`
  - **Content**: Agent Legibility Guide
  - **Topics**: Core Rules, Examples, Why It Matters, Active Monitoring, Grounding & Uncertainty
  - **User Questions**: "What can you tell me about Agent Legibility Guide?", "How does Core Rules work?", "How does Examples work?"

- **File**: `docs/skills/SKILLS_GUIDE.md`
  - **Content**: Agent Skills Guide
  - **Topics**: Discovery, Skill Structure, SKILL.md, Prompting Behavior, Commands
  - **User Questions**: "What can you tell me about Agent Skills Guide?", "How does Discovery work?", "How does Skill Structure work?"

- **File**: `docs/skills/AGENT_SKILLS_SPEC_IMPLEMENTATION.md`
  - **Content**: Agent Skills Spec Implementation
  - **Topics**: Implemented Behavior, Supported `SKILL.md` Fields, Validation Rules, Discovery Precedence, Deliberate Non-Support
  - **User Questions**: "What can you tell me about Agent Skills Spec Implementation?", "How does Implemented Behavior work?", "How does Supported `SKILL.md` Fields work?"

- **File**: `docs/protocols/ATIF_TRAJECTORY_FORMAT.md`
  - **Content**: Agent Trajectory Interchange Format (ATIF)
  - **Topics**: Overview, Configuration, Schema, VT Code Event Mapping, Implementation
  - **User Questions**: "What can you tell me about Agent Trajectory Interchange Format (ATIF)?", "How does Overview work?", "How does Configuration work?"

- **File**: `docs/a2a/README.md`
  - **Content**: Agent2Agent (A2A) Protocol Support
  - **Topics**: Overview, Architecture, Usage, JSON-RPC API Reference, Error Handling
  - **User Questions**: "What can you tell me about Agent2Agent (A2A) Protocol Support?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/styling/anstyle-crates-research.md`
  - **Content**: Anstyle Git/LS Crates Research & Vtcode Styling Improvements
  - **Topics**: Overview, Crate Analysis, Current Vtcode Styling Architecture, Recommended Improvements, Implementation Priority
  - **User Questions**: "What can you tell me about Anstyle Git/LS Crates Research & Vtcode Styling Improvements?", "How does Overview work?", "How does Crate Analysis work?"

- **File**: `docs/harness/ARCHITECTURAL_INVARIANTS.md`
  - **Content**: Architectural Invariants
  - **Topics**: 1. Layer Dependency Rules, 2. File Size Limits, 3. Naming Conventions, 4. Structured Logging, 5. No `unwrap()`
  - **User Questions**: "What can you tell me about Architectural Invariants?", "How does 1. Layer Dependency Rules work?", "How does 2. File Size Limits work?"

- **File**: `docs/styling/ARCHITECTURE.md`
  - **Content**: Architecture: Anstyle Integration in Vtcode
  - **Topics**: System Architecture Diagram, Data Flow: Style Parsing and Application, Module Dependencies, Effect Support Matrix, InlineTextStyle Evolution
  - **User Questions**: "What can you tell me about Architecture: Anstyle Integration in Vtcode?", "How does System Architecture Diagram work?", "How does Data Flow: Style Parsing and Application work?"

- **File**: `docs/analysis/BLOATY_ANALYSIS.md`
  - **Content**: Bloaty Analysis Report for vtcode
  - **Topics**: Overview, Binary Size Summary, Release-fast Binary Analysis (32 MiB), Debug Binary Analysis (84 MiB), Recommendations
  - **User Questions**: "What can you tell me about Bloaty Analysis Report for vtcode?", "How does Overview work?", "How does Binary Size Summary work?"

- **File**: `docs/blog/building-vt-code-a-year-in.md`
  - **Content**: Building VT Code, a year in
  - **Topics**: The model shelf, September 2026, Benchmarks, with honest framing, The roadmap shipped, The harness matters more than the model, Context engineering got real numbers
  - **User Questions**: "What can you tell me about Building VT Code, a year in?", "How does The model shelf, September 2026 work?", "How does Benchmarks, with honest framing work?"

- **File**: `docs/audits/code-review-2026-08-07.md`
  - **Content**: Code Review + Fixes — 2026-08-07
  - **Topics**: Completed fixes, Decomposition: `core/loop_detector.rs` → `core/loop_detector/mod.rs` + `normalization.rs`, Intentionally not changed, Verification summary, Status
  - **User Questions**: "What can you tell me about Code Review + Fixes — 2026-08-07?", "How does Completed fixes work?", "How does Decomposition: `core/loop_detector.rs` → `core/loop_detector/mod.rs` + `normalization.rs` work?"

- **File**: `docs/audits/code-review-2026-08-10.md`
  - **Content**: Code Review + Fixes — 2026-08-10
  - **Topics**: Findings and disposition, Compatibility and false-positive filtering, Refactoring guard rails, Verification
  - **User Questions**: "What can you tell me about Code Review + Fixes — 2026-08-10?", "How does Findings and disposition work?", "How does Compatibility and false-positive filtering work?"

- **File**: `docs/project/code-review-action-plan.md`
  - **Content**: Code Review Action Plan
  - **Topics**: MEDIUM SEVERITY, LOW SEVERITY, STYLE / QUALITY (bulk fixes), PRIORITY ORDER
  - **User Questions**: "What can you tell me about Code Review Action Plan?", "How does MEDIUM SEVERITY work?", "How does LOW SEVERITY work?"

- **File**: `docs/context/context_engineering.md`
  - **Content**: Context Engineering in VT Code
  - **Topics**: Overview, Context Engineering vs Prompt Engineering, VT Code's Three Context Primitives, Accuracy Optimization Loop, Core Principles
  - **User Questions**: "What can you tell me about Context Engineering in VT Code?", "How does Overview work?", "How does Context Engineering vs Prompt Engineering work?"

- **File**: `docs/harness/CORE_BELIEFS.md`
  - **Content**: Core Beliefs
  - **Topics**: 1. Humans Steer, Agents Execute, 2. Repository as System of Record, 3. Progressive Disclosure, 4. Agent Legibility Over Human Aesthetics, 5. Enforce Invariants, Not Implementations
  - **User Questions**: "What can you tell me about Core Beliefs?", "How does 1. Humans Steer, Agents Execute work?", "How does 2. Repository as System of Record work?"

- **File**: `docs/analysis/DRY_DUPLICATION_AUDIT.md`
  - **Content**: DRY/KISS Duplication Audit
  - **Topics**: Scope, Implemented Refactors, Duplication Intentionally Left Alone, Expected Benefits, Verification
  - **User Questions**: "What can you tell me about DRY/KISS Duplication Audit?", "How does Scope work?", "How does Implemented Refactors work?"

- **File**: `docs/harness/exec-plans/active/001-tui-runloop-decomposition.md`
  - **Content**: EP-001: TUI Runloop and Slash Command Decomposition
  - **Topics**: Goal, Context, Steps, Decision Log, Retrospective
  - **User Questions**: "What can you tell me about EP-001: TUI Runloop and Slash Command Decomposition?", "How does Goal work?", "How does Context work?"

- **File**: `docs/harness/EXEC_PLANS.md`
  - **Content**: Execution Plans
  - **Topics**: Why Exec Plans?, Exec Plans vs Planning Workflow, Directory Structure, Mandatory Sections, Template
  - **User Questions**: "What can you tell me about Execution Plans?", "How does Why Exec Plans? work?", "How does Exec Plans vs Planning Workflow work?"

- **File**: `docs/features/FILE_REFERENCE.md`
  - **Content**: File Reference Feature (@-Symbol)
  - **Topics**: Overview, Usage, UI Design, Implementation Details, Benefits
  - **User Questions**: "What can you tell me about File Reference Feature (@-Symbol)?", "How does Overview work?", "How does Usage work?"

- **File**: `docs/features/GPU_POD_MANAGER.md`
  - **Content**: GPU Pod Manager
  - **Topics**: Overview, Commands, Behavior Notes, Testing
  - **User Questions**: "What can you tell me about GPU Pod Manager?", "How does Overview work?", "How does Commands work?"

- **File**: `docs/harness/INDEX.md`
  - **Content**: Harness Engineering Knowledge Base
  - **Topics**: Purpose, File Index, Cross-References, Navigation, Maintaining Freshness
  - **User Questions**: "What can you tell me about Harness Engineering Knowledge Base?", "How does Purpose work?", "How does File Index work?"

- **File**: `docs/huggingface/index.md`
  - **Content**: Hugging Face Inference Providers Integrations
  - **Topics**: Featured Integrations, About This Directory, Overview, Configuration, Resources
  - **User Questions**: "What can you tell me about Hugging Face Inference Providers Integrations?", "How does Featured Integrations work?", "How does About This Directory work?"

- **File**: `docs/installation/README.md`
  - **Content**: Installation Guide
  - **Topics**: Quick Install, Supported AI Providers, Troubleshooting, Uninstall, Additional Resources
  - **User Questions**: "What can you tell me about Installation Guide?", "How does Quick Install work?", "How does Supported AI Providers work?"

- **File**: `docs/installation/DEVELOPERS.md`
  - **Content**: Installer Development Guide
  - **Topics**: Overview, Platform Detection, Release Binaries, GitHub Releases Setup, Testing Installers
  - **User Questions**: "What can you tell me about Installer Development Guide?", "How does Overview work?", "How does Platform Detection work?"

- **File**: `docs/protocols/KITTY_KEYBOARD_PROTOCOL_RESTORATION.md`
  - **Content**: Kitty Keyboard Protocol Restoration
  - **Topics**: Overview, Architecture, Files Modified/Restored, Configuration, Data Flow
  - **User Questions**: "What can you tell me about Kitty Keyboard Protocol Restoration?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/protocols/LANGUAGE_SUPPORT.md`
  - **Content**: Language Support in VT Code
  - **Topics**: Semantic Understanding, Tree-sitter Security Parsing (Bash), Syntax Highlighting
  - **User Questions**: "What can you tell me about Language Support in VT Code?", "How does Semantic Understanding work?", "How does Tree-sitter Security Parsing (Bash) work?"

- **File**: `docs/build-with-claude/migrating-to-claude-opus-5.md`
  - **Content**: Migrating Claude models in VT Code
  - **Topics**: Quick navigation, Model comparison, Where to make changes in VT Code, Migrating to Claude Fable 5 / Claude Mythos 5, Migrating to Claude Opus 5
  - **User Questions**: "What can you tell me about Migrating Claude models in VT Code?", "How does Quick navigation work?", "How does Model comparison work?"

- **File**: `docs/installation/NATIVE_INSTALLERS.md`
  - **Content**: Native Installers
  - **Topics**: macOS & Linux (Shell), Windows (PowerShell)
  - **User Questions**: "What can you tell me about Native Installers?", "How does macOS & Linux (Shell) work?", "How does Windows (PowerShell) work?"

- **File**: `docs/skills/NATIVE_PLUGIN_IMPLEMENTATION.md`
  - **Content**: Native Plugin System Implementation Summary
  - **Topics**: Overview, What Was Implemented, How to Use, Testing, Build Verification
  - **User Questions**: "What can you tell me about Native Plugin System Implementation Summary?", "How does Overview work?", "How does What Was Implemented work?"

- **File**: `docs/skills/NATIVE_PLUGIN_GUIDE.md`
  - **Content**: Native Plugin System for VT Code
  - **Topics**: Overview, What are Native Plugins?, Plugin Architecture, Plugin ABI, Creating a Plugin
  - **User Questions**: "What can you tell me about Native Plugin System for VT Code?", "How does Overview work?", "How does What are Native Plugins? work?"

- **File**: `docs/protocols/OPEN_RESPONSES.md`
  - **Content**: Open Responses Specification Conformance
  - **Topics**: Conformance Overview, What is Open Responses?, Implementation Details, Conformance Levels, Response Object Structure
  - **User Questions**: "What can you tell me about Open Responses Specification Conformance?", "How does Conformance Overview work?", "How does What is Open Responses? work?"

- **File**: `docs/project/PLAN-loop-engineering.md`
  - **Content**: PLAN Loop Engineering
  - **Topics**: Principles, Lifecycle, Evidence-bounded replanning, Cross-references
  - **User Questions**: "What can you tell me about PLAN Loop Engineering?", "How does Principles work?", "How does Lifecycle work?"

- **File**: `docs/pty/PTY_ANSI_HANDLING.md`
  - **Content**: PTY Output ANSI Handling
  - **Topics**: Overview, Architecture, ANSI Parser Implementation, Data Flow, Testing
  - **User Questions**: "What can you tell me about PTY Output ANSI Handling?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/pty/PTY_PIPE_INFRASTRUCTURE.md`
  - **Content**: PTY and Pipe Infrastructure
  - **Topics**: Overview, Module Structure, Usage Examples, Process Group Management, Security Features
  - **User Questions**: "What can you tell me about PTY and Pipe Infrastructure?", "How does Overview work?", "How does Module Structure work?"

- **File**: `docs/harness/prompt-architecture.md`
  - **Content**: Prompt Architecture
  - **Topics**: Cache-stable segments, Assembly order, Few-shot management (Section 18.3.3), Tool description contract (Section 18.3.4)
  - **User Questions**: "What can you tell me about Prompt Architecture?", "How does Cache-stable segments work?", "How does Assembly order work?"

- **File**: `docs/harness/QUALITY_SCORE.md`
  - **Content**: Quality Score
  - **Topics**: Snapshot, Dimensions, Scoring Method, LLM System, Tool System
  - **User Questions**: "What can you tell me about Quality Score?", "How does Snapshot work?", "How does Dimensions work?"

- **File**: `docs/installation/QUICK_REFERENCE.md`
  - **Content**: Quick Reference
  - **Topics**: Install, Uninstall, Verify, Troubleshooting, API Keys
  - **User Questions**: "What can you tell me about Quick Reference?", "How does Install work?", "How does Uninstall work?"

- **File**: `docs/styling/quick-reference.md`
  - **Content**: Quick Reference: Anstyle Crates
  - **Topics**: anstyle-git Syntax, anstyle-ls Syntax, Git Config Color Syntax, Vtcode Integration Points, Cheat Sheet: Common Patterns
  - **User Questions**: "What can you tell me about Quick Reference: Anstyle Crates?", "How does anstyle-git Syntax work?", "How does anstyle-ls Syntax work?"

- **File**: `docs/harness/SESSION_LOG_REVIEW.md`
  - **Content**: Session Log Review
  - **Topics**: 2026-08-16 | Checkpoints 912–917 diagnostics audit, 2026-08-12 | Planning wire-catalog collapse (turns 912–913), 2026-08-12 | Checkpoints 862–911 stabilization, 2026-08-11 | Harness token-waste and startup-noise audit, 2026-08-11 | Harness core logic audit (DRY/KISS, brittleness, long-running stability)
  - **User Questions**: "What can you tell me about Session Log Review?", "How does 2026-08-16 | Checkpoints 912–917 diagnostics audit work?", "How does 2026-08-12 | Planning wire-catalog collapse (turns 912–913) work?"

- **File**: `docs/features/SHELL_SNAPSHOT.md`
  - **Content**: Shell Environment Snapshot
  - **Topics**: Problem, Solution, Usage, Architecture, Excluded Environment Variables
  - **User Questions**: "What can you tell me about Shell Environment Snapshot?", "How does Problem work?", "How does Solution work?"

- **File**: `docs/skills/SKILL_AUTHORING_GUIDE.md`
  - **Content**: Skill Authoring Guide
  - **Topics**: Create a Skill, Write `SKILL.md`, Purpose, Workflow, Resources
  - **User Questions**: "What can you tell me about Skill Authoring Guide?", "How does Create a Skill work?", "How does Write `SKILL.md` work?"

- **File**: `docs/skills/CONTAINER_GUIDE.md`
  - **Content**: Skill Container API Guide
  - **Topics**: Basic Usage, Advanced Usage, Builder Pattern, Validation, Serialization
  - **User Questions**: "What can you tell me about Skill Container API Guide?", "How does Basic Usage work?", "How does Advanced Usage work?"

- **File**: `docs/skills/SKILL_TOOL_USAGE.md`
  - **Content**: Skill Tool Usage
  - **Topics**: Discovery Queries, Prompt Integration, Resource Loading, Storage Locations
  - **User Questions**: "What can you tell me about Skill Tool Usage?", "How does Discovery Queries work?", "How does Prompt Integration work?"

- **File**: `docs/styling/styling_integration.md`
  - **Content**: Styling Integration: anstyle-crossterm
  - **Topics**: Overview, Architecture, Components, Usage Examples, Benefits
  - **User Questions**: "What can you tell me about Styling Integration: anstyle-crossterm?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/styling/STYLING_QUICK_START.md`
  - **Content**: Styling Quick Start Guide
  - **Topics**: For CLI Output, For TUI Widgets, Unified Theme, Color Reference, Common Patterns
  - **User Questions**: "What can you tell me about Styling Quick Start Guide?", "How does For CLI Output work?", "How does For TUI Widgets work?"

- **File**: `docs/harness/TECH_DEBT_TRACKER.md`
  - **Content**: Tech Debt Tracker
  - **Topics**: Priority Levels, Status Values, Debt Items, How to Add a New Item, How to Resolve an Item
  - **User Questions**: "What can you tell me about Tech Debt Tracker?", "How does Priority Levels work?", "How does Status Values work?"

- **File**: `docs/huggingface/vtcode.md`
  - **Content**: VT Code
  - **Topics**: Overview, Configuration, Supported Models, Features with HF Integration, Common Use Cases
  - **User Questions**: "What can you tell me about VT Code?", "How does Overview work?", "How does Configuration work?"

- **File**: `docs/environment/ALLOWED_COMMANDS_REFERENCE.md`
  - **Content**: VT Code Allowed Commands Reference
  - **Topics**: Overview, Command Categories, Blocked Commands (Dangerous Operations), Environment Variables Preserved, Configuration
  - **User Questions**: "What can you tell me about VT Code Allowed Commands Reference?", "How does Overview work?", "How does Command Categories work?"

- **File**: `docs/environment/ENVIRONMENT_SETUP_GUIDE.md`
  - **Content**: VT Code Environment Setup and PATH Visibility Guide
  - **Topics**: Overview, VT Code storage environment, How VT Code Manages Environment Variables, Command Execution Paths, Verifying Your Environment Setup
  - **User Questions**: "What can you tell me about VT Code Environment Setup and PATH Visibility Guide?", "How does Overview work?", "How does VT Code storage environment work?"

- **File**: `docs/styling/RATATUI_FAQ_INTEGRATION.md`
  - **Content**: VT Code Integration of Ratatui FAQ Best Practices
  - **Topics**: Overview, FAQ Topics Applied, New Documentation, Code Comments, Testing Improvements
  - **User Questions**: "What can you tell me about VT Code Integration of Ratatui FAQ Best Practices?", "How does Overview work?", "How does FAQ Topics Applied work?"

- **File**: `docs/project/vtcode-project-info.md`
  - **Content**: VT Code Project Info
  - **Topics**: What is VT Code?, Architecture
  - **User Questions**: "What can you tell me about VT Code Project Info?", "How does What is VT Code? work?", "How does Architecture work?"

- **File**: `docs/sandbox/SANDBOX_DEEP_DIVE.md`
  - **Content**: VT Code Sandbox Deep Dive
  - **Topics**: Design Philosophy, Sandbox Policies, Platform-Specific Implementations, Security Features, Debug Tooling
  - **User Questions**: "What can you tell me about VT Code Sandbox Deep Dive?", "How does Design Philosophy work?", "How does Sandbox Policies work?"

- **File**: `docs/sandbox/SANDBOX_FIELD_GUIDE.md`
  - **Content**: VT Code Sandbox Field Guide
  - **Topics**: The Three-Question Model, Boundaries, Policy, Lifecycle, Platform-Specific Implementation
  - **User Questions**: "What can you tell me about VT Code Sandbox Field Guide?", "How does The Three-Question Model work?", "How does Boundaries work?"

- **File**: `docs/styling/INDEX.md`
  - **Content**: VT Code Styling System - Complete Documentation Index
  - **Topics**: Quick Navigation, Implementation Status, Document Guide, Quick Reference, Key Crates
  - **User Questions**: "What can you tell me about VT Code Styling System - Complete Documentation Index?", "How does Quick Navigation work?", "How does Implementation Status work?"

- **File**: `docs/protocols/XDG_DIRECTORY_SPECIFICATION.md`
  - **Content**: VT Code user directories
  - **Topics**: Directory categories, Platform roots, Legacy migration and rollback, Compatibility boundaries, References
  - **User Questions**: "What can you tell me about VT Code user directories?", "How does Directory categories work?", "How does Platform roots work?"

- **File**: `docs/styling/README.md`
  - **Content**: Vtcode Styling System Documentation
  - **Topics**: Files, Quick Summary, Architecture Overview, Dependencies, Related Code Locations
  - **User Questions**: "What can you tell me about Vtcode Styling System Documentation?", "How does Files work?", "How does Quick Summary work?"

- **File**: `docs/reference/webmcp.md`
  - **Content**: WebMCP deployment reference
  - **Topics**: Select a deployment, Pair either published page, Origin trial and browser support, Verification checklist
  - **User Questions**: "What can you tell me about WebMCP deployment reference?", "How does Select a deployment work?", "How does Pair either published page work?"

- **File**: `docs/protocols/ZED_EXTENSION_FILE_SEARCH.md`
  - **Content**: Zed Extension File Search Integration
  - **Topics**: Overview, New Commands, Architecture, File Structure, API Reference
  - **User Questions**: "What can you tell me about Zed Extension File Search Integration?", "How does Overview work?", "How does New Commands work?"

- **File**: `docs/harness/ZEN_ALIGNMENT.md`
  - **Content**: Zen Alignment for VT Code
  - **Topics**: Scope, Full Principle Mapping (All 19), Baseline Metrics (2026-03-03), Rollout, Verification Commands
  - **User Questions**: "What can you tell me about Zen Alignment for VT Code?", "How does Scope work?", "How does Full Principle Mapping (All 19) work?"

- **File**: `docs/ansi/ANSTYLE_CROSSTERM_IMPROVEMENTS.md`
  - **Content**: anstyle-crossterm Integration Improvements
  - **Topics**: Overview, Key Improvements, Color Mapping Behavior, Architecture Flow, Usage Patterns
  - **User Questions**: "What can you tell me about anstyle-crossterm Integration Improvements?", "How does Overview work?", "How does Key Improvements work?"

- **File**: `docs/ansi/ANSTYLE_PARSE_INTEGRATION.md`
  - **Content**: anstyle-parse Integration Guide
  - **Topics**: Step 1: Add Dependency, Step 2: Create Parser Wrapper Module, Step 3: Update Module Exports, Step 4: Replace Manual Parser in PTY, Step 5: Update ANSI Stripping
  - **User Questions**: "What can you tell me about anstyle-parse Integration Guide?", "How does Step 1: Add Dependency work?", "How does Step 2: Create Parser Wrapper Module work?"

- **File**: `docs/examples/background-subagent-demo.md`
  - **Content**: background-subagent-demo.md
  - **User Questions**: "What can you tell me about background-subagent-demo.md?"

- **File**: `docs/project/ROADMAP.md`
  - **Content**: vtcode Development Roadmap
  - **Topics**: **Recently Completed - Major Breakthroughs**, **High Priority - SWE-bench Performance Optimization**, Medium Priority, Low Priority, Implementation Notes
  - **User Questions**: "What can you tell me about vtcode Development Roadmap?", "How does **Recently Completed - Major Breakthroughs** work?", "How does **High Priority - SWE-bench Performance Optimization** work?"

### Performance & Optimization

- **File**: `docs/benchmarks/CHART_GUIDE.md`
  - **Content**: Benchmark Chart Quick Reference
  - **Topics**: Current Chart, Chart Breakdown, Key Insights, Generating Your Own Charts, Comparing Models
  - **User Questions**: "What can you tell me about Benchmark Chart Quick Reference?", "How does Current Chart work?", "How does Chart Breakdown work?"

- **File**: `docs/benchmarks/COMPARISON.md`
  - **Content**: Benchmark Comparison
  - **Topics**: Current Results, Planned Comparisons, Expected Performance Ranges, How to Add New Results, Analysis Framework
  - **User Questions**: "What can you tell me about Benchmark Comparison?", "How does Current Results work?", "How does Planned Comparisons work?"

- **File**: `docs/benchmarks/SUMMARY.md`
  - **Content**: Benchmark Summary
  - **Topics**: Quick Reference, Latest Results, How to Run, Visualization, Files
  - **User Questions**: "What can you tell me about Benchmark Summary?", "How does Quick Reference work?", "How does Latest Results work?"

- **File**: `docs/benchmarks/VISUALIZATION.md`
  - **Content**: Benchmark Visualization Guide
  - **Topics**: Chart Components, Generating Charts, Chart Interpretation, Example Charts, Comparing Multiple Models
  - **User Questions**: "What can you tell me about Benchmark Visualization Guide?", "How does Chart Components work?", "How does Generating Charts work?"

- **File**: `docs/benchmarks/HUMANEVAL_2025-10-22.md`
  - **Content**: HumanEval Benchmark Results - October 22, 2025
  - **Topics**: Executive Summary, Configuration, Results, Methodology, Analysis
  - **User Questions**: "What can you tell me about HumanEval Benchmark Results - October 22, 2025?", "How does Executive Summary work?", "How does Configuration work?"

- **File**: `docs/benchmarks/README.md`
  - **Content**: VT Code Benchmarks
  - **Topics**: Overview, Next.js AI Agent Evaluations, HumanEval Benchmark, Contributing, References
  - **User Questions**: "What can you tell me about VT Code Benchmarks?", "How does Overview work?", "How does Next.js AI Agent Evaluations work?"

- **File**: `docs/benchmarks/performance_benchmarks.md`
  - **Content**: VT Code Performance Benchmarks
  - **Topics**: Overview, Benchmark Methodology, Performance Results, Memory Profile, Clone Operation Audit
  - **User Questions**: "What can you tell me about VT Code Performance Benchmarks?", "How does Overview work?", "How does Benchmark Methodology work?"

- **File**: `docs/benchmarks/BENCHMARK_COMPARISON.md`
  - **Content**: Which Benchmark Should You Use?
  - **Topics**: Quick Answer, Detailed Comparison, HumanEval, MBPP (Mostly Basic Python Problems), SWE-bench (Software Engineering Benchmark)
  - **User Questions**: "What can you tell me about Which Benchmark Should You Use??", "How does Quick Answer work?", "How does Detailed Comparison work?"

### Security & Safety

- **File**: `docs/security/SECURITY_DOCUMENTATION_INDEX.md`
  - **Content**: Security Documentation Index
  - **Topics**: Core Documentation, Security Features by Layer, Configuration, Reporting Security Issues
  - **User Questions**: "What can you tell me about Security Documentation Index?", "How does Core Documentation work?", "How does Security Features by Layer work?"

- **File**: `docs/security/UNSAFE_INVENTORY.md`
  - **Content**: Unsafe Code Inventory
  - **Topics**: Snapshot: 2026-08-10, Native-plugin site breakdown, Change rule
  - **User Questions**: "What can you tell me about Unsafe Code Inventory?", "How does Snapshot: 2026-08-10 work?", "How does Native-plugin site breakdown work?"

- **File**: `docs/security/SECURITY_MODEL.md`
  - **Content**: VT Code Security Model
  - **Topics**: Overview, Security Architecture Diagram, Security Layers, Threat Model, Attack Scenarios
  - **User Questions**: "What can you tell me about VT Code Security Model?", "How does Overview work?", "How does Security Architecture Diagram work?"

- **File**: `docs/security/SECURITY_WEB_FETCH.md`
  - **Content**: Web Fetch Security & Malicious URL Prevention
  - **Topics**: Overview, Security Layers, Error Messages, Implementation Details, Testing
  - **User Questions**: "What can you tell me about Web Fetch Security & Malicious URL Prevention?", "How does Overview work?", "How does Security Layers work?"

### Tools & Functionality

- **File**: `docs/tools/JUSTIFICATION_SYSTEM.md`
  - **Content**: Agent Justification System
  - **Topics**: Overview, Architecture, Data Flow, Data Persistence, Integration Points
  - **User Questions**: "What can you tell me about Agent Justification System?", "How does Overview work?", "How does Architecture work?"

- **File**: `docs/tools/defuddle_fetch.md`
  - **Content**: Defuddle Fetch (now `web_fetch` `format=markdown`)
  - **Topics**: Guard Rails, Related Tools
  - **User Questions**: "What can you tell me about Defuddle Fetch (now `web_fetch` `format=markdown`)?", "How does Guard Rails work?", "How does Related Tools work?"

- **File**: `docs/tools/EDITOR_CONFIG.md`
  - **Content**: External Editor Configuration
  - **Topics**: Overview, Configuration, Editor Detection Order, Usage Examples, Setting Your Preferred Editor
  - **User Questions**: "What can you tell me about External Editor Configuration?", "How does Overview work?", "How does Configuration work?"

- **File**: `docs/tools/GIT_COMMAND_EXECUTION.md`
  - **Content**: Git Command Execution Policy
  - **Topics**: Overview, Supported Operations, Usage Examples, Flags and Parameters, Security Model
  - **User Questions**: "What can you tell me about Git Command Execution Policy?", "How does Overview work?", "How does Supported Operations work?"

- **File**: `docs/tools/GIT_QUICK_REFERENCE.md`
  - **Content**: Git Commands - Quick Reference
  - **Topics**: Allowed Operations, Blocked Operations, Common Workflows, Error Messages, Notes
  - **User Questions**: "What can you tell me about Git Commands - Quick Reference?", "How does Allowed Operations work?", "How does Blocked Operations work?"

- **File**: `docs/tools/max_tokens_support.md`
  - **Content**: Per-Call Output Limits
  - **Topics**: Overview, Supported Public Tools, Examples, Result Shape, Recommended Budgets
  - **User Questions**: "What can you tell me about Per-Call Output Limits?", "How does Overview work?", "How does Supported Public Tools work?"

- **File**: `docs/tools/PROMPT_CACHING_GUIDE.md`
  - **Content**: Prompt Caching Guide
  - **Topics**: Global Settings, Provider Overrides, Prefix Stability Rules, Usage Telemetry, Validation & Testing
  - **User Questions**: "What can you tell me about Prompt Caching Guide?", "How does Global Settings work?", "How does Provider Overrides work?"

- **File**: `docs/tools/TOOL_SEARCH.md`
  - **Content**: Tool Search Integration
  - **Topics**: Overview, Anthropic configuration, Tool Search Algorithms, API Usage, Response Block Types
  - **User Questions**: "What can you tell me about Tool Search Integration?", "How does Overview work?", "How does Anthropic configuration work?"

- **File**: `docs/tools/TOOL_SPECS.md`
  - **Content**: VT Code Tool Specifications
  - **Topics**: Public Profiles, Default Tools, Advanced Code Search, File Inspection, Platform Profiles
  - **User Questions**: "What can you tell me about VT Code Tool Specifications?", "How does Public Profiles work?", "How does Default Tools work?"

- **File**: `docs/tools/web_fetch_security.md`
  - **Content**: Web Fetch Tool Security Configuration
  - **Topics**: Security Modes, Dynamic Configuration Files, Inline Configuration, HTTPS Enforcement, Security Best Practices
  - **User Questions**: "What can you tell me about Web Fetch Tool Security Configuration?", "How does Security Modes work?", "How does Dynamic Configuration Files work?"

- **File**: `docs/tools/web_search.md`
  - **Content**: Web Search Tool
  - **Topics**: Usage, Output, Configuration, Guard Rails, Related Tools
  - **User Questions**: "What can you tell me about Web Search Tool?", "How does Usage work?", "How does Output work?"

### User Workflows & Commands

- **File**: `docs/user-guide/commands.md`
  - **Content**: Command Reference
  - **Topics**: Search, Storage diagnostics, File operations, Session resume and forks, Quick Actions in Chat Input
  - **User Questions**: "What can you tell me about Command Reference?", "How does Search work?", "How does Storage diagnostics work?"

- **File**: `docs/user-guide/interactive-mode.md`
  - **Content**: Interactive Mode Reference
  - **Topics**: Keyboard Shortcuts, Fullscreen Rendering, Scheduled Prompts And Reminders, Vim Mode, Prompt Suggestions, Tasks, and Jobs
  - **User Questions**: "What can you tell me about Interactive Mode Reference?", "How does Keyboard Shortcuts work?", "How does Fullscreen Rendering work?"

## Enhanced Trigger Questions

### Core Capabilities & Features
- "What can VT Code do?"
- "What are VT Code's main features?"
- "How does VT Code compare to other AI coding tools?"
- "What makes VT Code unique?"
- "Can VT Code handle multiple programming languages?"
- "Does VT Code support real-time collaboration?"
- "How does VT Code handle large codebases efficiently?"
- "What are the different system prompt modes (minimal, lightweight, etc.)?"
- "How can I reduce token usage with tool documentation modes?"

### Workflows & Agent Behavior
- "How do I use the planning workflow?"
- "How do I use the @ symbol to reference files in my messages?"
- "How do I use the /files slash command to browse my workspace?"
- "What is the Decision Ledger and how does it help with coherence?"
- "How does the agent handle long-running conversations?"

### Security & Reliability
- "What security layers does VT Code implement?"
- "How does VT Code ensure shell command safety?"
- "What is the 5-layer security model in VT Code?"
- "How do tool policies and human-in-the-loop approvals work?"
- "How does the circuit breaker prevent cascading failures?"
- "Is my code and data safe with VT Code?"

### Integrations & Protocols
- "How do I use VT Code inside the Zed editor?"
- "What is the Agent Client Protocol (ACP) and how is it used?"
- "What is the Agent2Agent (A2A) protocol?"
- "How does VT Code conform to the Open Responses specification?"
- "How do I configure Model Context Protocol (MCP) servers?"
- "What are lifecycle hooks and how do I configure them?"

### Local Models & Providers
- "Can I use VT Code with local models via Ollama?"
- "How do I integrate VT Code with LM Studio?"
- "Which AI providers are supported (OpenAI, Anthropic, Gemini, etc.)?"
- "How do I set up OpenRouter with VT Code?"
- "How can I use Hugging Face Inference Providers?"

### Getting Started & Setup
- "How do I install VT Code?"
- "How do I get started with VT Code?"
- "How do I set up VT Code for the first time?"
- "What do I need to get started?"
- "How do I configure API keys?"
- "Which LLM provider should I choose?"
- "How do I configure VT Code for my workflow?"
- "What are the most common keyboard shortcuts?"

### Development & Maintenance
- "How do I build VT Code from source?"
- "How do I run the test suite?"
- "How do I add a new tool to VT Code?"
- "How do I debug agent behavior or tool execution?"
- "How do I run the performance benchmarks?"
- "How do I update the self-documentation map?"
- "How do I contribute to the VT Code project?"
- "What is the release process for VT Code?"
- "How do I manage multi-crate dependencies in this workspace?"

## VT Code Feature Categories

### Core Capabilities
- **Multi-LLM Provider Support**: OpenAI, Anthropic, Google, DeepSeek, xAI, OpenRouter, Moonshot AI, Ollama, LM Studio
- **Terminal Interface**: Modern TUI with mouse support, text selection, and streaming output
- **Workspace Management**: Automatic project indexing, fuzzy file discovery, and context curation
- **Tool System**: Modular, extensible tool architecture with 53+ specialized tools
- **Security**: Enterprise-grade safety with tree-sitter-bash validation, sandboxing, and policy controls
- **Agent Protocols**: Support for ACP, A2A, and Open Responses for cross-tool interoperability

## Additional Resources

### External Documentation
- **Repository**: https://github.com/vinhnx/vtcode
- **Crate**: https://crates.io/crates/vtcode
- **VS Code Extension**: Open VSX and VS Code Marketplace

---

**Note**: This enhanced documentation map is designed for VT Code's self-documentation system. When users ask questions about VT Code itself, the system should fetch this document and use it to provide accurate, up-to-date information about VT Code's capabilities and features.
