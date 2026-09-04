# Repository tooling baseline

> **Scope: internal/maintainer-oriented.** External contributors only need
> the short version in [CONTRIBUTING.md](../../CONTRIBUTING.md): a Python 3.9
> or newer `python3` on `PATH`.

The repository's own gates and test runners are not hermetic: they run under
whichever `python3` and `bash` the host provides. This page records the floor
each one must run on, why that floor is where it is, and where CI holds it.

## Python

The repository's Python tooling requires Python 3.9 or newer, uniformly —
nothing in the tree needs more. The floor was briefly 3.11 because
`scripts/cli-timeout-policy.py` and its tests read the CLI execution contracts
with `tomllib`, stdlib only from 3.11 (RUE-1509); since RUE-1524 those
contracts are consumed as a Buck-materialized JSON twin, derived at build time
via `//crates/rue-toml2json`, and the floor is a uniform 3.9 again.

A stock Mac meets the floor: macOS ships `/usr/bin/python3` as 3.9.6, and
3.9 is chosen precisely so that interpreter is enough — nothing to install.
The runners' own interpreters are comfortably above the floor —
`ubuntu-latest` and `ubuntu-24.04-arm` provide 3.12.3, `macos-15` provides
3.14.6 — so a premerge-tier target using a construct newer than 3.9 would stay
green on them and fail on a stock developer machine, which is what
`//:cli-timeout-policy-validation` did while it needed 3.11 (RUE-1509). CI
therefore holds the floor by running the tooling under it: the `fmt`,
`linux-premerge`, and `ci-contract` jobs install Python 3.9 with
`actions/setup-python` before any gate runs, so a construct newer than the
floor fails there the way it fails on a stock Mac (RUE-1936 retired the static
scanner that approximated this with a curated table of constructs).

This floor governs the interpreter that runs repository tooling. It is not the
Python number in [build-cache.md](build-cache.md), which records what the
pinned remote worker image ships for the Buck prelude's rustc wrapper — a
different interpreter running different code. The remote-execution canary
builds; it does not run these tests.

## Shell

Shell has the same shape of floor and a stricter one. macOS ships GNU Bash
3.2.57 as `/bin/bash` and will not ship a GPLv3 one, so a `#!/usr/bin/env bash`
script has to run on 3.2 — on a stock Mac and on a `macos-*` runner that is the
interpreter it gets. `scripts/validate-shell-bash-baseline.py` holds two checks
to that floor, and neither covers the other. A curated construct table names
Bash 4+ spellings, which is what catches a script that parses on 3.2 and then
misbehaves: `mapfile` exiting 127 (RUE-1506), `${v:1:-1}` silently answering
empty. A `bash -n` pass parses every discovered shell script — `#!/bin/sh` and
bare `.sh` included, since a syntax error is one in any shell — and that is
what catches a file which does not parse at all: RUE-1511 shipped an unbalanced
double quote inside a multi-line command substitution, and the table called it
clean because a syntax error is not a construct (RUE-1512).

The two halves need different interpreters and get them in different places.
An unbalanced quote is a syntax error on every bash, so the Linux `fmt` job's
run catches that class on every pull request. `;;&`, `;&`, and `coproc` are a
syntax error only on 3.2, so that half is authoritative on the `macos-15` leg
of `native-platforms`, which runs the gate with `--require-baseline-bash` and
fails if its `/bin/bash` is not a 3.x. Every run says which interpreter parsed
and whether that was the baseline, so a weaker run cannot read as a stronger
one, and an empty discovery set fails rather than passing as a clean tree.

Annotate a reviewed table exception with `# bash-baseline-ok: <reason>`. The
parse check has its own, `# bash-parse-ok: <reason>`, which is file-level
because a parse failure names where the parser gave up rather than where the
mistake is. It exists for one real case: `bash -n` parses without executing, so
it never runs a `shopt`, and a file enabling `extglob` and then using `@(a|b)`
runs correctly on 3.2.57 while `bash -n` on that same interpreter rejects it.
A genuine syntax error is fixed, not annotated.
