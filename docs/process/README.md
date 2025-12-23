# Rue Development Process

This directory documents how we develop the Rue compiler. Whether you're a human contributor or an AI assistant, these documents describe the workflow we follow.

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
| - | [issue-tracking.md](issue-tracking.md) | `bd` CLI | Track work with beads |

## Feature Types

We distinguish between two types of work:

### Small Features
- Touch 1-3 files
- Single concept (new operator, syntax sugar)
- Completable in one session
- No preview gate needed

**Workflow**: Plan → bd issue → Implement → Review → Commit

### Large Features
- Touch many files across crates
- Multiple implementation phases
- May span multiple sessions
- Require ADR and preview gate

**Workflow**: Plan → ADR + bd epic → (Phase 1: Implement → Review → Commit) → ... → Stabilize

## Key Concepts

### ADRs (Architecture Decision Records)
Design documents for large features. See [../designs/README.md](../designs/README.md).

### Preview Features
Gating mechanism for incomplete features. Allows merging partial work to main without breaking stable functionality. See [ADR-0005](../designs/0005-preview-features.md).

### Issue Tracking (bd)
We use [beads](https://github.com/steveyegge/beads) for all issue tracking. See [issue-tracking.md](issue-tracking.md).

### Specification
Language semantics are formally documented in [../spec/](../spec/). Changes to language behavior require spec updates.

## Tools

- **Buck2**: Build system (`./buck2 build`, `./buck2 test`)
- **Jujutsu**: Version control (`jj status`, `jj commit`)
- **bd**: Issue tracking (`bd create`, `bd ready`)
- **Claude Code**: AI assistant with `/plan`, `/implement`, etc.

## Getting Started

1. **Find work**: `bd ready` shows unblocked issues
2. **Claim it**: `bd update <id> --status in_progress`
3. **Follow the process**: Use the documents and commands above
4. **Ship it**: Review, commit, close the issue
