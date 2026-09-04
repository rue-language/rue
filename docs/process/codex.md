# Codex sessions

> **Scope: internal/maintainer-oriented.** This page holds the guidance that
> applies only to a Codex session working in Rue. Everything else lives in
> [AGENTS.md](../../AGENTS.md), which Codex reads first and which points here.

## Division of labor

- For end-to-end issue implementation, default to one Luna High implementation
  agent when that capability is available. The coordinating agent investigates,
  writes a precise brief, reviews the returned change, integrates it, and owns
  publication and cleanup.
- Add a separate adversarial reviewer only when the change is architectural,
  cross-phase, unsafe, security-sensitive, or otherwise unusually risky.
- Do not delegate routine polling, repository-instruction discovery, or reading
  `AGENTS.md`. Do not create multiple implementation agents for the same
  scope.

## Checkout

A Codex-managed Git worktree uses Git natively for the entire workflow, per
the Git checkout flow in `AGENTS.md`. Do not recreate its work in a Jujutsu
workspace merely to follow the maintainer workflow. A sandboxed `gh auth
status` or network failure is not authoritative on the host; retry the
required read or publication operation with host access before reporting an
authentication blocker.
