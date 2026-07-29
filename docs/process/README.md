# Rue Development Process

> **Audience: internal maintainers and AI agents.** These documents describe the
> maintainers' internal operations — Linear-based issue tracking, Jujutsu (`jj`)
> version control, Claude Code workflows, and merge-queue management. They are
> **not required for external contributions**. If you are contributing through
> GitHub, read [CONTRIBUTING.md](../../CONTRIBUTING.md) instead — ordinary Git
> and GitHub issues/PRs are the supported path, and nothing here is needed to
> submit a change.

This directory documents how the maintainers develop the Rue compiler.

## Overview

Our development process follows this cycle:

```
Idea → Plan → Implement → Review → Commit → (Stabilize)
```

Each step has a corresponding document in this directory and a Claude Code command that automates it.

## Quick Reference

| Step | Document | Command | Purpose |
|------|----------|---------|---------|
| Plan | [planning.md](planning.md) | `/plan` | Design features, create ADRs and issues |
| Implement | [implementation.md](implementation.md) | `/implement` | Write code, tests, and spec updates |
| Review | [code-review.md](code-review.md) | `/code-review` | Check quality before committing |
| Commit | [committing.md](committing.md) | `/commit` | Create well-formed commits |
| - | [ci.md](ci.md) | - | Maintain required CI and its pinned tools |
| - | [profiling.md](profiling.md) | - | Build symbolized executables for native profiling |
| - | [compiler-facade.md](compiler-facade.md) | - | Review compiler API and tooling-view changes |
| - | [tutorial.md](tutorial.md) | - | Maintain tutorial outline, style, and snippet checks |
| - | [issue-tracking.md](issue-tracking.md) | Linear MCP tools | Track work with Linear |

## Feature Types

We distinguish between two types of work:

### Small Features
- Touch 1-3 files
- Single concept (new operator, syntax sugar)
- Completable in one session
- No preview gate needed

**Workflow**: Plan → Linear issue → Implement → Review → Commit

### Large Features
- Touch many files across crates
- Multiple implementation phases
- May span multiple sessions
- Require ADR and preview gate

**Workflow**: Plan → ADR + Linear epic → (Phase 1: Implement → Review → Commit) → ... → Stabilize

## Key Concepts

### ADRs (Architecture Decision Records)
Design documents for large features. See [../designs/README.md](../designs/README.md).

### Preview Features
Gating mechanism for incomplete features. Allows merging partial work to main without breaking stable functionality. See [ADR-0005](../designs/0005-preview-features.md).

### Issue Tracking (Linear)
We use [Linear](https://linear.app) (team "Rue") for all issue tracking. See [issue-tracking.md](issue-tracking.md).

### Specification
Language semantics are formally documented in [../spec/](../spec/). Changes to language behavior require spec updates.

## Tools

- **Buck2**: Build system (`./buck2 build`, `./buck2 test`)
- **Jujutsu**: Version control (`jj status`, `jj commit`)
- **Linear**: Issue tracking (via the Linear MCP tools)
- **Claude Code**: AI assistant with `/plan`, `/implement`, etc.

## Getting Started

1. **Find work**: list `Todo`/`Backlog` issues in the Rue team via the Linear MCP tools
2. **Claim it**: `save_issue` with state "In Progress" and assignee "me"
3. **Follow the process**: Use the documents and commands above
4. **Ship it**: Review, commit, mark the issue Done
