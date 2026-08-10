# Fuzz Failure Reporting

The nightly fuzz workflow (`.github/workflows/fuzz.yml`) files the crashes it
finds into **Linear**, in the **Rue** team, one issue per distinct crash. GitHub
Issues remains as a clearly-marked fallback. This page documents the mechanism,
the one manual setup step, and how to triage what it files.

> **Setup required.** Until the `LINEAR_API_KEY` secret exists, the nightly
> workflow still reports — but into GitHub Issues, not Linear. See
> [Manual setup](#manual-setup-linear_api_key) below.

## Why this changed (RUE-802)

Fuzz crashes used to be filed by an inline `actions/github-script` block that
kept **one** open GitHub issue labelled `fuzz-crash`. Every later crash found
while that issue stayed open was appended to it as a comment. Two consequences:

- Rue plans from Linear, so automatically-found bugs sat in a tracker nobody
  triages from.
- Dedup was by *label*, not by crash. A miscompile found in August landed as a
  comment on an unrelated stack-overflow issue from July, and closing that issue
  reset the bucket. Recurrence and novelty were indistinguishable.

Reporting is now `scripts/fuzz-report-failure.py`, keyed on a per-crash
fingerprint.

## How it works

```
fuzz targets crash ──► crates/rue-fuzz/crashes/  ──► scripts/fuzz-report-failure.py ──► Linear (Rue team)
   (rue-fuzz,           crash-<target>-…​.txt + .meta         fingerprint + dedup            └─ GitHub Issues (fallback)
    rue-oracle-diff)    oracle-diff-seed-<n>.rue
```

1. **Collect.** Every reproducer under the crash directory is read back. The
   `rue-fuzz` harness writes a `.txt` input with a `.txt.meta` sibling recording
   `target`, `signature`, and `outcome`; `rue-oracle-diff` writes a `.rue` repro
   whose leading `// reason:` comment carries the signature. Both are parsed. A
   reproducer with no metadata still reports, with a degraded signature — an
   unexplained crash file is still a crash.

2. **Fingerprint.** The crash identity is
   `sha256(target + "\n" + normalize(signature))[:16]`. Normalization erases
   what varies between two sightings of one bug and nothing that distinguishes
   two bugs:

   | Erased | Why |
   |---|---|
   | `0x…` addresses | ASLR moves them every run |
   | Absolute path prefixes (the last three components are kept, so `rue-sema/src/check.rs` stays distinct from `rue-codegen/src/check.rs`) | the runner's checkout root is not the bug |
   | `file.rs:120:9` line/column | any unrelated edit moves them |
   | `seed 918273` | the same disagreement reproduces at many seeds |
   | Digit runs of 4+ | byte counts, offsets, sizes |

   Short numbers survive on purpose: `signal 6` and `signal 11` are different
   bugs.

3. **Dedup.** The fingerprint is written into the issue description as a
   `Fuzz-Fingerprint: \`<hash>\`` line. Before filing, open (not
   completed/canceled) Rue-team issues are searched for that exact line — the
   labelled field, not the bare hash, so a commit prefix in an unrelated issue
   cannot match. A hit gets a recurrence comment naming the new run; a miss
   files a new issue.

4. **File.** New Linear issues carry the `Bug` and `found-by:autonomous` labels
   and a `[fuzz-crash]` title marker (Linear has no `fuzz-crash` label; the
   marker is what makes the class searchable). A label that cannot be resolved
   warns and is skipped — a crash filed with the wrong labels still gets
   triaged, one never filed does not.

If a step fails without leaving any reproducer (a wedged build, a timeout
killing the harness), one report per failed target is synthesized instead, so
that night is still visible.

## Backends and the fallback

| `LINEAR_API_KEY` | `GITHUB_TOKEN` | Behavior |
|---|---|---|
| set | either | Files in Linear (Rue team). |
| unset | set | Warns loudly, files in GitHub Issues with a "filed by the fallback path" banner in the body and the `fuzz-crash` label. Dedup is still per-fingerprint. |
| unset | unset | Exits non-zero; the workflow step goes red. |

The last row is deliberate. The failure this design guards against is *silence*:
a night whose crashes are found and then dropped. Reporting nothing and exiting
`0` is never an outcome.

This is also why `.github/workflows/fuzz.yml` keeps `permissions: issues: write`
even though the primary path no longer needs it — that permission is what makes
the fallback real.

## Manual setup: `LINEAR_API_KEY`

**This is a one-time step a repository admin (Steve) must do; it cannot be
scripted from here.** Until it is done, nightly crashes are filed as GitHub
Issues rather than Linear issues.

1. In Linear, open **Settings → Security & access → Personal API keys** (or
   <https://linear.app/settings/account/security>) and **Create key**. Label it
   e.g. `rue-fuzz-ci`. Copy the key — Linear shows it once.
   - The key must belong to a member of the **Rue** team with permission to
     create issues and comments there.
2. In GitHub, open the repository's **Settings → Secrets and variables →
   Actions → New repository secret**.
3. Name it exactly `LINEAR_API_KEY`, paste the key, and save.
4. Verify: run the **Fuzz Testing** workflow via `workflow_dispatch`. On a run
   that finds a crash, the reporting step logs `reporting via Linear`. If it
   logs the `LINEAR_API_KEY is not set` warning instead, the secret name or
   scope is wrong.

Rotating the key is the same flow; nothing in the repository pins its value.

## Running it by hand

`--dry-run` drives the real client code — query construction, label resolution,
dedup branching, payload assembly — against a synthesizing transport that prints
every request instead of sending it. No credentials are needed or used.

```bash
# What would this crash directory file?
scripts/fuzz-report-failure.py --dry-run --crash-dir crates/rue-fuzz/crashes

# What would a failed run with no surviving reproducer file?
scripts/fuzz-report-failure.py --dry-run --failed-targets lexer,emitter_aarch64
```

To file for real from a local run, export `LINEAR_API_KEY` and drop
`--dry-run`. `--backend linear` refuses to fall back to GitHub, which is what
you want when testing the Linear path specifically.

## Tests

`//:fuzz-report-tool-tests` (`scripts/test-fuzz-report-failure.py`) pins the
parts that decide whether a crash is reported once, twice, or not at all:
fingerprint stability against run-to-run noise, fingerprint separation between
distinct bugs, dedup producing a comment rather than a second issue, the create
payload's team/labels/marker/fingerprint line, backend selection including the
no-credentials failure, and the workflow actually invoking the script. The API
layer is injected (`Transport`), so no test touches the network.
