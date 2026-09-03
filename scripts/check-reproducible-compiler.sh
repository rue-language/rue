#!/usr/bin/env bash
# RUE-617: rebuild the Rue compiler from a relocated clean source tree and
# compare the complete artifact with the compiler already built by CI.
set -euo pipefail

for tool in awk cmp git readelf sha256sum strings tar; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'error: compiler reproducibility check requires %s\n' "$tool" >&2
        exit 1
    fi
done

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
temp_base="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
scratch="$(mktemp -d "$temp_base/rue-compiler-reproducibility-long-root.XXXXXX")"
candidate_root="$scratch/second-clean-checkout"
candidate_tmp="$scratch/tmp"
artifact_dir="$temp_base/rue-compiler-repro-artifacts"
mkdir -p "$candidate_root" "$candidate_tmp"
mkdir -p "$artifact_dir"
rm -f \
    "$artifact_dir/reference-rue" \
    "$artifact_dir/candidate-rue" \
    "$artifact_dir/diagnostics.txt" \
    "$artifact_dir/reference-readelf.txt" \
    "$artifact_dir/candidate-readelf.txt"

# Both builds must be cache-free. The ./buck2 wrapper links .buckconfig.local
# to an installed BuildBuddy config on a checkout's first execution command;
# such a link is moved aside for the duration (restored on exit, after a
# failure too) and the opt-out keeps the wrapper from recreating it. A
# hand-written .buckconfig.local is refused: the reference and the relocated
# candidate would build under different configuration. Toggling the file
# restarts the Buck daemon, which is expected here.
central_config="${RUE_BUILDBUDDY_CONFIG:-${XDG_CONFIG_HOME:-${HOME:-}/.config}/rue/buildbuddy.buckconfig}"
local_config="$repo_root/.buckconfig.local"
aside_config=""

cleanup() {
    if [ -x "$candidate_root/buck2" ]; then
        (
            cd "$candidate_root"
            ./buck2 --isolation-dir compiler-repro kill >/dev/null 2>&1 || true
        )
    fi
    if [ -n "$aside_config" ] && [ -L "$aside_config" ]; then
        mv "$aside_config" "$local_config"
    fi
    rm -rf "$scratch"
}
trap cleanup EXIT

if [ -L "$local_config" ] && [ "$(readlink "$local_config")" = "$central_config" ]; then
    aside_config="$scratch/buckconfig.local.aside"
    mv "$local_config" "$aside_config"
elif [ -e "$local_config" ] || [ -L "$local_config" ]; then
    printf 'error: .buckconfig.local would make the reference and archived candidate use different configuration\n' >&2
    printf '       move it aside before running the reproducibility check\n' >&2
    exit 1
fi
export RUE_NO_REMOTE_CACHE=1

materialize_source_tree() {
    if git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        if [ -n "$(git -C "$repo_root" status --porcelain --untracked-files=normal)" ]; then
            printf 'error: conventional Git checkout must be clean so HEAD matches the reference build inputs\n' >&2
            return 1
        fi
        # Actions checks out the exact synthetic PR commit. git archive keeps
        # the candidate independent of the first checkout's .git metadata and
        # excludes untracked/build outputs by construction.
        git -C "$repo_root" archive --format=tar HEAD | tar -xpf - -C "$candidate_root"
        return
    fi

    # Local development uses pure jj workspaces without a colocated .git. The
    # working-copy commit is already exported to jj's backing Git store, so use
    # the same immutable archive path CI exercises rather than copying live
    # filesystem bytes.
    if command -v jj >/dev/null 2>&1 && jj -R "$repo_root" root >/dev/null 2>&1; then
        local revision git_dir
        revision="$(jj -R "$repo_root" log -r @ --no-graph -T commit_id)"
        git_dir="$(jj -R "$repo_root" git root)"
        git --git-dir="$git_dir" archive --format=tar "$revision" |
            tar -xpf - -C "$candidate_root"
        return
    fi

    printf 'error: cannot materialize a tracked source tree (git or jj required)\n' >&2
    return 1
}

sha256() {
    sha256sum "$1" | awk '{print $1}'
}

build_id() {
    if command -v readelf >/dev/null 2>&1; then
        readelf -n "$1" 2>/dev/null | awk '/Build ID:/ { print $3; exit }'
    fi
}

assert_no_root_leak() {
    local binary="$1" root="$2" label="$3"
    # Do not use grep -m/-q here: with pipefail, an early grep exit can turn
    # strings' SIGPIPE into a false negative.
    if strings -a "$binary" | grep -F -- "$root" >/dev/null; then
        printf 'FAIL: %s embeds checkout path %s\n' "$label" "$root" >&2
        return 1
    fi
}

preserve_failure_artifacts() {
    mkdir -p "$artifact_dir"
    cp "$reference_binary" "$artifact_dir/reference-rue"
    cp "$candidate_binary" "$artifact_dir/candidate-rue"
    {
        printf 'reference: %s\n' "$reference_binary"
        printf 'reference sha256: %s\n' "$reference_hash"
        printf 'reference build-id: %s\n' "${reference_build_id:-none}"
        printf 'candidate: %s\n' "$candidate_binary"
        printf 'candidate sha256: %s\n' "$candidate_hash"
        printf 'candidate build-id: %s\n' "${candidate_build_id:-none}"
    } > "$artifact_dir/diagnostics.txt"
    if command -v readelf >/dev/null 2>&1; then
        readelf -h -S -n "$reference_binary" > "$artifact_dir/reference-readelf.txt" 2>&1 || true
        readelf -h -S -n "$candidate_binary" > "$artifact_dir/candidate-readelf.txt" 2>&1 || true
    fi
    printf '  failure artifacts: %s\n' "$artifact_dir" >&2
}

materialize_source_tree

# File mtimes are not declared compiler inputs. Make the relocated tree
# conspicuously different from actions/checkout before Buck fingerprints it.
find "$candidate_root" -type f -exec touch -t 203001010000 {} +

reference_binary="${RUE_REPRO_REFERENCE_BINARY:-}"
if [ -z "$reference_binary" ]; then
    reference_binary="$($repo_root/scripts/rue-bin --local-only --no-remote-cache)"
fi
if [ ! -f "$reference_binary" ]; then
    printf 'error: reference compiler does not exist: %s\n' "$reference_binary" >&2
    exit 1
fi

# Use one isolated daemon/output tree, disable both remote execution and remote
# cache reads, and perturb scheduling plus ambient process state. Dotslash and
# the pinned Buck/Rust inputs may reuse their download caches; compilation
# actions themselves must execute cold in this source root.
candidate_binary="$(
    cd "$candidate_root"
    umask 077
    env \
        LC_ALL=C \
        SOURCE_DATE_EPOCH=2000000000 \
        TMPDIR="$candidate_tmp" \
        TZ=Pacific/Honolulu \
        ./buck2 --isolation-dir compiler-repro build //crates/rue:rue \
            --local-only \
            --no-remote-cache \
            --num-threads 2 \
            --show-full-simple-output
)"
if [ ! -f "$candidate_binary" ]; then
    printf 'error: candidate compiler does not exist: %s\n' "$candidate_binary" >&2
    exit 1
fi

reference_hash="$(sha256 "$reference_binary")"
candidate_hash="$(sha256 "$candidate_binary")"
reference_build_id="$(build_id "$reference_binary")"
candidate_build_id="$(build_id "$candidate_binary")"

if ! cmp -s "$reference_binary" "$candidate_binary"; then
    first_diff="$(cmp -l "$reference_binary" "$candidate_binary" 2>/dev/null |
        awk 'NR == 1 { print $1; exit }' || true)"
    printf 'FAIL: independently rebuilt Rue compilers differ\n' >&2
    printf '  reference: %s (%s bytes, sha256 %s, build-id %s)\n' \
        "$reference_binary" "$(wc -c < "$reference_binary" | tr -d ' ')" \
        "$reference_hash" "${reference_build_id:-none}" >&2
    printf '  candidate: %s (%s bytes, sha256 %s, build-id %s)\n' \
        "$candidate_binary" "$(wc -c < "$candidate_binary" | tr -d ' ')" \
        "$candidate_hash" "${candidate_build_id:-none}" >&2
    if [ -n "$first_diff" ]; then
        printf '  first differing byte offset (1-based): %s\n' "$first_diff" >&2
    fi
    preserve_failure_artifacts
    exit 1
fi

if [ "$reference_build_id" != "$candidate_build_id" ]; then
    printf 'FAIL: compiler GNU build IDs differ: %s != %s\n' \
        "${reference_build_id:-none}" "${candidate_build_id:-none}" >&2
    preserve_failure_artifacts
    exit 1
fi

if ! assert_no_root_leak "$reference_binary" "$repo_root" "reference compiler" ||
    ! assert_no_root_leak "$reference_binary" "$candidate_root" "reference compiler" ||
    ! assert_no_root_leak "$candidate_binary" "$repo_root" "candidate compiler" ||
    ! assert_no_root_leak "$candidate_binary" "$candidate_root" "candidate compiler"; then
    preserve_failure_artifacts
    exit 1
fi

printf 'Rue compiler rebuild is byte-reproducible\n'
printf '  sha256:  %s\n' "$reference_hash"
printf '  build-id: %s\n' "${reference_build_id:-none}"
rmdir "$artifact_dir" 2>/dev/null || true
