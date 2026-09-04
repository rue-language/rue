#!/usr/bin/env python3
"""Supervise one `rue test` run for the `rue_test` build rule (RUE-2004).

Named a supervisor rather than a runner because the runner is the compiler:
`crates/rue-test-runner` is the compiler suite's own harness, and `rue test`
owns the run this script watches.

ADR-0083's boundary gives the build system exactly one input to the runner: the
declared candidate inventory. `rue_test` writes that inventory from its `srcs`
and assembles the whole compiler command line at analysis time, so everything
this script receives after `--` is already the argv to run. What is left here is
policy, and it is deliberately the only thing here:

  * the run's verdict is read from the NDJSON EVENT STREAM, never from the human
    rendering, which is not a contract (test-events.md, "Streams");
  * a nonzero exit (1 failures, 2 compile or runner error, 3 empty selection)
    fails the target;
  * a non-empty `run_finished.unimported_test_files` fails the target too,
    unless the rule set `allow_unimported`. `rue test` itself exits 0 with that
    warning on stderr, which is why a build rule that only forwarded the exit
    code would let a `*_tests.rue` nobody imports stay invisible — the gap this
    rule closes.

On any failure the script prints a reader-facing summary — the runner's own
stderr verbatim, then each non-passing test with its failure record and repro
argv, then the offending unimported paths. The machine surface decides; the
human surface is printed so the log explains itself.

`--expect-unimported` inverts the last check for the fixture negative control
(fixtures/rue-program/BUCK), the way `--expect-violation` inverts the derive
step's boundary check: the script succeeds if and only if the run would have
been failed for exactly those paths.
"""

import argparse
import json
import shlex
import subprocess
import sys

# Exit codes of `rue test` (docs/process/test-events.md, "Exit codes").
_EXIT_REASONS = {
    1: "a selected test failed, timed out, or crashed",
    2: "the run could not be performed (compile failure, ICE, or runner error)",
    3: "the selection was empty",
}


def parse_args(argv):
    parser = argparse.ArgumentParser(allow_abbrev=False)
    parser.add_argument("--allow-unimported", action="store_true")
    parser.add_argument("--expect-unimported", action="append", default=[])
    parser.add_argument("command", nargs="+")
    return parser.parse_args(argv)


def read_events(stdout):
    """The NDJSON stream as objects, plus any line that was not one.

    A line that is not an object is reported rather than skipped: under
    `--format json` stdout is the stream, so anything else on it means the
    surface this rule reads is not the surface the runner wrote.
    """
    events = []
    malformed = []
    for line in stdout.splitlines():
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            malformed.append(line)
            continue
        if isinstance(event, dict):
            events.append(event)
        else:
            malformed.append(line)
    return events, malformed


def describe_failure(test):
    """One non-passing `test_finished` event, rendered for a reader."""
    lines = ["  {} [{}]".format(test.get("id", "<no id>"), test.get("verdict"))]
    failure = test.get("failure") or {}
    if failure:
        lines.append(
            "    {}: {}".format(
                failure.get("kind", "<no kind>"),
                failure.get("message", ""),
            )
        )
        location = failure.get("location") or {}
        if location:
            lines.append(
                "    at {}:{}:{}".format(
                    location.get("file", "?"),
                    location.get("line", 0),
                    location.get("column", 0),
                )
            )
        if "left" in failure and "right" in failure:
            lines.append("    left:  {}".format(failure["left"]))
            lines.append("    right: {}".format(failure["right"]))
    for stream in ("stdout", "stderr"):
        capture = test.get(stream) or {}
        data = capture.get("data")
        if data:
            lines.append("    {}: {}".format(stream, data.rstrip("\n")))
    repro = test.get("repro")
    if repro:
        # The environment leads, the way a shell wants it: `repro` is argv
        # alone, and `repro_env` carries what the run owed to the environment
        # (RUE-2020). Only the value is quoted -- quoting the name half would
        # stop the shell reading the word as an assignment.
        assignments = [
            "{}={}".format(name, shlex.quote(value))
            for name, value in sorted((test.get("repro_env") or {}).items())
        ]
        lines.append(
            "    repro: {}".format(" ".join(assignments + [shlex.join(repro)]))
        )
    return lines


def describe_unimported(files):
    lines = []
    for entry in files:
        if entry.get("parse_failed"):
            lines.append(
                "  {} (could not be read or parsed)".format(entry.get("path"))
            )
        else:
            tests = entry.get("tests")
            lines.append(
                "  {} declares {} {} that no module in the compiled closure "
                "imports".format(
                    entry.get("path"),
                    tests,
                    "test" if tests == 1 else "tests",
                )
            )
    return lines


def report(command, result, reasons, run_finished, events):
    """The human-readable summary, printed only when the target fails."""
    print("rue_test: {}".format("; ".join(reasons)), file=sys.stderr)
    print("command: {}".format(shlex.join(command)), file=sys.stderr)
    if result.stderr:
        print("--- rue test stderr ---", file=sys.stderr)
        sys.stderr.write(result.stderr)
        if not result.stderr.endswith("\n"):
            sys.stderr.write("\n")
    failed = [
        event
        for event in events
        if event.get("event") == "test_finished" and event.get("verdict") != "pass"
    ]
    if failed:
        print("--- non-passing tests ---", file=sys.stderr)
        for test in failed:
            for line in describe_failure(test):
                print(line, file=sys.stderr)
    unimported = (run_finished or {}).get("unimported_test_files") or []
    if unimported:
        print("--- test files nothing imports ---", file=sys.stderr)
        for line in describe_unimported(unimported):
            print(line, file=sys.stderr)
        print(
            "add an @import for each file above to the root's closure, or set "
            "allow_unimported = True on the rue_test target",
            file=sys.stderr,
        )
    if run_finished:
        print(
            "summary: {} passed, {} failed, {} timed out, {} crashed in {} ms".format(
                run_finished.get("passed", 0),
                run_finished.get("failed", 0),
                run_finished.get("timeout", 0),
                run_finished.get("crash", 0),
                run_finished.get("wall_ms", 0),
            ),
            file=sys.stderr,
        )


def main(argv):
    args = parse_args(argv)
    command = args.command
    result = subprocess.run(command, capture_output=True, text=True, check=False)
    events, malformed = read_events(result.stdout)
    run_finished = next(
        (event for event in events if event.get("event") == "run_finished"),
        None,
    )

    reasons = []
    if result.returncode != 0:
        reasons.append(
            "rue test exited {} — {}".format(
                result.returncode,
                _EXIT_REASONS.get(result.returncode, "unknown status"),
            )
        )
    for line in malformed:
        reasons.append("stdout carried a line that is not one event: {!r}".format(line))
    if result.returncode == 0 and run_finished is None:
        reasons.append("the event stream carried no run_finished event")

    unimported = (run_finished or {}).get("unimported_test_files")
    if unimported is None and run_finished is not None:
        # The rule always passes --test-candidates, so the field is always
        # present; its absence means the inventory never reached the compiler.
        reasons.append(
            "run_finished carried no unimported_test_files: the declared "
            "inventory did not reach the compiler"
        )
        unimported = []
    unimported = unimported or []
    orphan_paths = sorted(entry.get("path") for entry in unimported)

    if args.expect_unimported:
        # Negative control: the run must be the one this rule fails, for
        # exactly these paths and for no other reason.
        expected = sorted(args.expect_unimported)
        problems = []
        if reasons:
            problems.append("the run failed for another reason: {}".format(reasons))
        if orphan_paths != expected:
            problems.append(
                "expected unimported {} but the run reported {}".format(
                    expected, orphan_paths
                )
            )
        for path in expected:
            if path not in result.stderr:
                problems.append("stderr never named {}".format(path))
        if problems:
            print(
                "rue_test negative control: {}".format("; ".join(problems)),
                file=sys.stderr,
            )
            report(command, result, ["negative control did not hold"], run_finished, events)
            return 1
        print(
            "rue_test negative control held: {} would fail the target".format(
                ", ".join(expected)
            )
        )
        return 0

    if orphan_paths and not args.allow_unimported:
        reasons.append(
            "declared test files nothing imports: {}".format(", ".join(orphan_paths))
        )

    if reasons:
        report(command, result, reasons, run_finished, events)
        return 1

    if orphan_paths:
        # allow_unimported: the warning still belongs in the log, since the
        # attribute suppresses the failure, not the finding.
        print("rue_test: test files nothing imports (allowed by this target):")
        for line in describe_unimported(unimported):
            print(line)
    print(
        "rue_test: {} passed in {} ms".format(
            run_finished.get("passed", 0),
            run_finished.get("wall_ms", 0),
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
