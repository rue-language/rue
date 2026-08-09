#!/bin/bash
#
# Setup script for Claude Code cloud environments (claude.ai/code).
#
# This file is NOT picked up automatically -- cloud environments only run
# whatever is pasted into the environment's "Setup script" field (Add/Edit
# cloud environment dialog at claude.ai/code). Paste the body of this file
# there once per environment; it then runs on the first session and gets
# cached (see the "Setup scripts" section of
# https://code.claude.com/docs/en/cloud-environments), so this isn't repeated
# on every session.
#
# What it fixes: Rue's `./buck2`, `./btd`, `./reindeer`, `./mdbook`,
# `./tailwindcss`, and `./zola` wrappers are all DotSlash manifests
# (`#!/usr/bin/env dotslash`). DotSlash itself isn't part of the cloud VM
# image, so every one of those wrappers fails with "dotslash: command not
# found" until it's installed once per environment. This script installs it
# and, if the Rue checkout is already on disk when the script runs, warms
# Buck2's caches so the first real build in the session doesn't pay for it.
#
# Per the cloud-environment setup-script contract this must always exit 0 --
# a nonzero exit fails session startup outright -- so every step below is
# best-effort and errors are swallowed rather than propagated.
set -uo pipefail

echo "==> Installing DotSlash"
if command -v dotslash >/dev/null 2>&1; then
    echo "    already installed: $(dotslash --version)"
else
    # DotSlash ships on crates.io, and Rust (rustc/cargo) is part of the
    # standard cloud VM image, so this needs no extra network allowlisting
    # beyond the default Trusted level. Takes roughly a minute on a 4-vCPU
    # session. Retry once in case of a transient registry hiccup, but never
    # fail the setup script over it -- a session that starts without
    # DotSlash is still useful for non-build work, and Claude can install it
    # by hand mid-session if this step didn't stick.
    if ! cargo install --locked dotslash; then
        echo "    first attempt failed, retrying once" >&2
        cargo install --locked dotslash || echo "    dotslash install failed; ./buck2 and friends won't work this session" >&2
    fi
fi

if command -v dotslash >/dev/null 2>&1; then
    echo "==> Looking for a Rue checkout to warm Buck2's caches"
    # The setup script may run before or after the repo is cloned depending
    # on the environment, so search for it rather than assuming a path.
    # Bounded so a slow/large filesystem can't eat the 5-minute setup budget.
    repo_dir="$(timeout 10 find / -maxdepth 6 -name .buckroot -not -path '/proc/*' 2>/dev/null | head -n1 || true)"

    if [[ -n "$repo_dir" ]]; then
        repo_root="$(dirname "$repo_dir")"
        echo "    found $repo_root"
        (
            cd "$repo_root" || exit 0
            # Materializes the pinned buck2 binary into DotSlash's
            # content-addressed cache (~/.cache/dotslash). That cache lives
            # outside the repo clone and is keyed by hash, so it survives a
            # fresh checkout in a later session as long as buck2-bin's pin
            # is unchanged.
            echo "==> Fetching the pinned buck2 binary"
            timeout 60 ./buck2 --version || echo "    buck2 fetch failed; it will retry on first use" >&2

            # Warms the Rust toolchain download and third-party crate build
            # under buck-out/. This only pays off if this checkout is the
            # same one a later session resumes rather than a fresh clone;
            # harmless either way, and bounded so it can't blow the setup
            # budget even on a cold cache.
            echo "==> Warming a full compiler build (best-effort, bounded)"
            timeout 240 ./buck2 build //crates/rue:rue || echo "    warm build didn't finish in time; first build in-session will be slower" >&2
        )
    else
        echo "    no checkout found yet -- skipping the warm-up. ./buck2 bootstraps itself on first use once DotSlash is installed."
    fi
fi

exit 0
