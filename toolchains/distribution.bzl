"""Hermetic toolchain distributions, fetched from their origin instead of the cache.

Every pinned toolchain payload Rue downloads — the Rust `rustc`, `rust-std`,
Clippy, and rustfmt components, and the Zig distribution — used to be a
`prelude//:http_archive`. That rule's extraction step is an ordinary Buck
action, so with `[buck2] default_allow_cache_upload = true` (which the shared
BuildBuddy configuration sets, because ordinary Rust actions need it) the
*extracted tree* was uploaded to the remote CAS and later served back from it.

Those trees are the only artifacts in Rue's graph made of tens of thousands of
tiny files: the Zig 0.16.0 `x86_64-linux` distribution alone is 19,546 files
and 341 MiB, of which 19,544 files and 170 MiB are small enough to travel in
`BatchReadBlobs` rather than the ByteStream API. Materializing one is therefore
the only thing Rue ever asks BuildBuddy to answer with a batch of thousands of
blobs, and from 2026-09-03 16:34 UTC those answers started coming back cut off
mid-stream — `missing grpc-status trailer, stream was terminated without a
final status`, or `Unexpected EOF decoding stream` — which buck2 reports as
`materialize_inputs_failed` and does not retry, because neither maps to a
retryable gRPC code. Across the merge-group failures of 2026-09-03/04 the
failing artifact was one of exactly four action digests, and all four are these
distributions (RUE-2003).

`toolchain_distribution` keeps them out of that path entirely:

- the archive is fetched with `download_file`, which buck2's materializer
  serves from the origin URL and never through the CAS;
- the extraction declares `allow_cache_upload = False`, so no action-cache
  entry is ever written for the tree and no machine can be served one. That
  flag is an explicit opt-out of `default_allow_cache_upload`, which buck2
  documents as "default for per-action `allow_cache_upload`, to make it
  opt-out instead of opt-in".

The archives are SHA-pinned, so extracting locally produces the same bytes the
cache would have returned; nothing about reproducibility or hermeticity
changes. What is given up is sharing the *unpacked* tree between machines.

The cost is real and lands in a place worth naming, because an action's output
digest is what keys its consumers: with no action-cache entry, buck2 can only
learn that digest by running the action, so every lane with a fresh `buck-out`
now fetches and extracts every distribution its graph reaches — including a
lane that would otherwise have been served entirely from cache and materialized
nothing. Measured across one CI run that is about 237 MiB and ten to twenty
seconds per such lane. docs/process/build-cache.md has the before/after table
and the argument for paying it.

Execution placement is deliberately left at buck2's default, matching what
`http_archive` did for a SHA256-pinned archive: ordinary commands carry
`--prefer-local` from the `./buck2` wrapper and extract on the runner, while
`--prefer-remote` still extracts on the worker, so the remote-execution canary
is unchanged.
"""

def _strip_components(strip_prefix: str) -> int:
    return len([c for c in strip_prefix.split("/") if c != ""])

def _toolchain_distribution_impl(ctx: AnalysisContext) -> list[Provider]:
    archive = ctx.actions.declare_output("archive.tar.xz")
    ctx.actions.download_file(
        archive.as_output(),
        ctx.attrs.url,
        sha256 = ctx.attrs.sha256,
    )

    # The output directory keeps the target's own name, so a buck-out path
    # names the distribution it came from exactly as `http_archive` did.
    output = ctx.actions.declare_output(ctx.label.name, dir = True)

    # `sh -c SCRIPT NAME ARG...` binds $0 to NAME and $1.. to the arguments, so
    # every path reaches tar as a positional word and needs no quoting in the
    # command string. Actions run from the project root and buck2 substitutes
    # project-relative paths, which is what `-C` and `-f` want.
    #
    # `--strip-components=N PREFIX` mirrors the prelude: the prefix is also a
    # member selector, so a distribution that ever grew a sibling top-level
    # directory would be extracted the same way it is today rather than
    # silently gaining files.
    ctx.actions.run(
        cmd_args(
            "/bin/sh",
            "-c",
            'set -eu; mkdir -p "$1"; exec tar -J -x -f "$2" -C "$1" --strip-components="$3" "$4"',
            "toolchain_distribution",
            output.as_output(),
            archive,
            str(_strip_components(ctx.attrs.strip_prefix)),
            ctx.attrs.strip_prefix,
        ),
        category = "toolchain_distribution",
        identifier = ctx.label.name,
        # The point of this rule. See the module docstring.
        allow_cache_upload = False,
    )

    return [DefaultInfo(default_output = output)]

toolchain_distribution = rule(
    impl = _toolchain_distribution_impl,
    attrs = {
        "sha256": attrs.string(
            doc = "SHA256 of the archive. Pins the payload; buck2 verifies it.",
        ),
        "strip_prefix": attrs.string(
            doc = "Top-level directory inside the archive, removed on extraction.",
        ),
        "url": attrs.string(doc = "Origin URL of the `.tar.xz` archive."),
    },
)
