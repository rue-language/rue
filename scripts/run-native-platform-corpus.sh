#!/usr/bin/env bash
# Run every declaratively platform-scoped spec and CLI case for this host.
#
# RUE_PLATFORM_CASE_SELECTION=native is implemented by the shared manifest
# layer, so adding `only_on` to a case automatically adds it to this lane.
# Target-independent cases and automatic examples remain in the Linux corpus.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_dir="$(cd "$script_dir/.." && pwd)"
cd "$repo_dir"

rue_binary="$(scripts/rue-bin)"
export RUE_BINARY="$rue_binary"
export RUE_PLATFORM_CASE_SELECTION=native

RUE_SPEC_CASES="$repo_dir/crates/rue-spec/cases" \
  ./buck2 run //crates/rue-spec:rue-spec -- --quiet

RUE_CLI_CASE_TIER=premerge \
RUE_CLI_CASES="$repo_dir/crates/rue-cli-tests/cases" \
RUE_EXAMPLES_DIR="$repo_dir/examples" \
RUE_REPO_DIR="$repo_dir/cli-test-fixtures" \
RUE_STD_DIR="$repo_dir/std" \
  ./buck2 run //crates/rue-cli-tests:rue-cli-tests -- --quiet
