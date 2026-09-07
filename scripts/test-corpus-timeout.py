#!/usr/bin/env python3
"""Focused tests for corpus-timeout cleanup deadline races."""

import importlib.util
import signal
import sys
from pathlib import Path


def load_timeout_module():
    path = Path(__file__).with_name("corpus-timeout.py")
    spec = importlib.util.spec_from_file_location("corpus_timeout", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load corpus-timeout.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeProcess:
    pid = 99


def run_cleanup(clock_values, group_values):
    module = load_timeout_module()
    clock = iter(clock_values)
    groups = iter(group_values)
    sleeps = []
    signals = []
    original_monotonic = module.time.monotonic
    original_sleep = module.time.sleep
    original_group_exists = module.group_exists
    original_signal_group = module.signal_group
    try:
        module.time.monotonic = lambda: next(clock)

        def sleep(seconds):
            if seconds < 0:
                raise AssertionError("cleanup requested a negative sleep")
            sleeps.append(seconds)

        module.time.sleep = sleep
        module.group_exists = lambda _group: next(groups)
        module.signal_group = lambda group, number: signals.append((group, number))
        module.terminate_group(FakeProcess())
    finally:
        module.time.monotonic = original_monotonic
        module.time.sleep = original_sleep
        module.group_exists = original_group_exists
        module.signal_group = original_signal_group
    return signals, sleeps


def test_term_deadline_crossing() -> None:
    # The clock crosses TERM's deadline between the loop condition and the
    # sleep calculation used by the old implementation.
    signals, sleeps = run_cleanup((0.0, 0.5, 1.1, 2.0, 3.1), (True, True, True))
    expected = [(99, signal.SIGTERM), (99, signal.SIGKILL)]
    if signals != expected:
        raise AssertionError("TERM-edge signals were {!r}, expected {!r}".format(signals, expected))
    if not sleeps or any(seconds < 0 for seconds in sleeps):
        raise AssertionError("TERM-edge cleanup slept {!r}".format(sleeps))


def test_kill_deadline_crossing() -> None:
    # Keep both calls in the TERM iteration positive, then cross KILL's
    # deadline between its loop condition and sleep calculation.
    signals, sleeps = run_cleanup(
        (0.0, 0.5, 0.6, 1.1, 2.0, 2.5, 3.1),
        (True, True, True, True, True),
    )
    expected = [(99, signal.SIGTERM), (99, signal.SIGKILL)]
    if signals != expected:
        raise AssertionError("KILL-edge signals were {!r}, expected {!r}".format(signals, expected))
    if len(sleeps) < 2 or any(seconds < 0 for seconds in sleeps):
        raise AssertionError("KILL-edge cleanup slept {!r}".format(sleeps))


def test_group_exit_skips_forced_kill() -> None:
    signals, sleeps = run_cleanup((0.0, 0.1), (False,))
    expected = [(99, signal.SIGTERM)]
    if signals != expected:
        raise AssertionError("normal-exit signals were {!r}, expected {!r}".format(signals, expected))
    if sleeps:
        raise AssertionError("normal-exit cleanup slept {!r}".format(sleeps))


if __name__ == "__main__":
    try:
        test_term_deadline_crossing()
        test_kill_deadline_crossing()
        test_group_exit_skips_forced_kill()
    except (AssertionError, RuntimeError, StopIteration) as error:
        print("FAIL: {}".format(error), file=sys.stderr)
        sys.exit(1)
    print("corpus-timeout: deadline-race checks passed")
