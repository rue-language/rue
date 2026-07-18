# Required CI

The `CI` workflow (`.github/workflows/ci.yml`) supplies the required pull-request
and merge-group checks.

## Triggers

`CI` and `Sanitizer` run on `pull_request` and `merge_group` only (plus
`workflow_dispatch` for manual re-validation). There is deliberately no
`push: [trunk]` trigger (RUE-1006): trunk only advances through the merge
queue, and the merge_group run's checks are attached to the exact commit that
lands, so a post-merge trunk run would re-test an identical tree. Do not
re-add a push trigger to these workflows; `Benchmarks` keeps its push trigger
because per-commit measurement on trunk is its purpose.

The build jobs use the shared BuildBuddy remote action cache when the
`BUILDBUDDY_API_KEY` secret is available (merge_group runs; fork PRs build
cold) — see `docs/process/build-cache.md` for the availability rules,
including why the linux-x64 test lane stays cache-free. Containers executed by that workflow must use a reviewed,
human-readable release tag and the immutable OCI index digest for that tag. The
repository gate `//:required-ci-container-pin-validation` rejects a moving
`latest` image reference, and the normal `./test.sh` run includes that gate.

## Updating actionlint

1. Find the latest stable release and review its notes and Dockerfile:

   ```bash
   gh release view --repo rhysd/actionlint
   gh api 'repos/rhysd/actionlint/contents/Dockerfile?ref=v<VERSION>' \
     --jq .content | base64 --decode
   ```

   In particular, confirm that the image still installs ShellCheck. actionlint
   can use the image's `/usr/local/bin/shellcheck` executable to check every
   `run:` block while it discovers every workflow under `.github/workflows/`.

2. Resolve the reviewed release tag to its multi-platform OCI index digest:

   ```bash
   docker buildx imagetools inspect docker.io/rhysd/actionlint:<VERSION>
   ```

   Update the image in `ci.yml` as
   `rhysd/actionlint:<VERSION>@sha256:<INDEX-DIGEST>`. Keep both parts: the tag
   records what humans reviewed, while the digest fixes the bytes CI executes.

3. Verify that the pinned image contains ShellCheck, run actionlint exactly as
   CI does, then run the repository policy and its focused regression tests:

   ```bash
   docker run --rm --entrypoint /usr/local/bin/shellcheck \
     rhysd/actionlint:<VERSION>@sha256:<INDEX-DIGEST> --version
   docker run --rm -v "$PWD:/repo:ro" -w /repo \
     rhysd/actionlint:<VERSION>@sha256:<INDEX-DIGEST> \
     -color -shellcheck=/usr/local/bin/shellcheck
   ./buck2 test //:required-ci-container-pin-validation \
     //:required-ci-container-pin-tool-tests
   ```

Both container invocations must finish successfully. Together they verify that
the image contains ShellCheck and that actionlint checks the `run:` blocks with
that binary while linting all discovered workflows.
