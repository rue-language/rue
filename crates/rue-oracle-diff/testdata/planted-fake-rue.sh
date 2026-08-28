#!/usr/bin/env bash
set -euo pipefail

: "${RUE_ORACLE_DIFF_FAKE_PROGRAM:?missing fake compiled program}"
: "${RUE_ORACLE_DIFF_OPT_LOG:?missing optimization log}"

output=""
optimization=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -O0|-O1|-O2|-O3)
            [ -z "$optimization" ] || exit 2
            optimization="$1"
            shift
            ;;
        -o)
            [ "$#" -ge 2 ] || exit 2
            output="$2"
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
[ -n "$output" ] || exit 2
[ -n "$optimization" ] || exit 2
printf '%s\n' "$optimization" >>"$RUE_ORACLE_DIFF_OPT_LOG"
cp "$RUE_ORACLE_DIFF_FAKE_PROGRAM" "$output"
chmod +x "$output"
