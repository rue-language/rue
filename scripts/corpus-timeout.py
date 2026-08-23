#!/usr/bin/env python3
"""Run a corpus harness with a portable, process-group timeout.

This is deliberately a small action tool rather than a lookup of ``timeout``
from PATH.  The Buck target that builds it is an input to every corpus action,
so changing the timeout implementation invalidates the action cache entry.

Usage:
    corpus-timeout.py SECONDS TIMEOUT_MARKER COMMAND [ARG ...]

The marker is private wrapper plumbing.  It is created only when the command
actually exceeds its deadline, which lets the shell wrapper distinguish that
case from a harness that legitimately exits 124 without changing the command's
stdout or stderr.
"""

import math
import os
import signal
import subprocess
import sys
import time
from typing import List, Optional


# A wedged harness gets this long to handle TERM before the whole process group
# is forcibly killed.  Keep the bound short because this is the action's outer
# deadline, not a harness-level cleanup policy.
TERM_GRACE_SECONDS = 1.0
KILL_GRACE_SECONDS = 1.0


def usage() -> int:
    print(
        "usage: corpus-timeout.py SECONDS TIMEOUT_MARKER COMMAND [ARG ...]",
        file=sys.stderr,
    )
    return 2


def parse_seconds(value: str) -> float:
    try:
        seconds = float(value)
    except ValueError as error:
        raise ValueError("timeout must be a finite number greater than zero") from error
    if not math.isfinite(seconds) or seconds <= 0:
        raise ValueError("timeout must be a finite number greater than zero")
    return seconds


def signal_group(process_group: int, signal_number: int) -> bool:
    """Signal a process group, tolerating a group that already exited."""
    try:
        os.killpg(process_group, signal_number)
    except (PermissionError, ProcessLookupError):
        return False
    return True


def group_exists(process_group: int) -> bool:
    """Return whether the process group still has a member."""
    try:
        os.killpg(process_group, 0)
    except (PermissionError, ProcessLookupError):
        return False
    return True


def terminate_group(process: subprocess.Popen, first_signal: int = signal.SIGTERM) -> None:
    """Signal the harness group, then KILL it after a bounded grace period."""
    process_group = process.pid
    signal_group(process_group, first_signal)

    deadline = time.monotonic() + TERM_GRACE_SECONDS
    while time.monotonic() < deadline:
        if not group_exists(process_group):
            return
        time.sleep(min(0.05, deadline - time.monotonic()))

    # Do not stop at the leader: a harness can leave descendants running after
    # it handles TERM, so the forced cleanup always addresses the whole group.
    signal_group(process_group, signal.SIGKILL)
    deadline = time.monotonic() + KILL_GRACE_SECONDS
    while time.monotonic() < deadline and group_exists(process_group):
        time.sleep(min(0.05, deadline - time.monotonic()))


def shell_status(returncode: int) -> int:
    """Map a signal termination to the status a shell would report."""
    if returncode < 0:
        return 128 + (-returncode)
    return returncode


def write_timeout_marker(path: str) -> None:
    try:
        with open(path, "w", encoding="ascii") as marker:
            marker.write("timed out\n")
    except OSError:
        # The timeout status remains authoritative even if diagnostic plumbing
        # cannot be recorded.  The wrapper still emits its generic failure.
        pass


def reap_after_termination(process: subprocess.Popen) -> None:
    """Reap the leader without allowing cleanup to become unbounded."""
    try:
        process.wait(timeout=KILL_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        pass


def wait_for_process(
    process: subprocess.Popen, seconds: float, pending_signal: List[Optional[int]]
) -> Optional[int]:
    """Wait in short intervals so wrapper cancellation is handled promptly."""
    deadline = time.monotonic() + seconds
    while True:
        if pending_signal[0] is not None:
            return None
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            # The deadline is a race boundary. A child that completed at the
            # same instant must keep its real status and must not be marked as
            # a timeout or killed.
            completed = process.poll()
            if completed is not None:
                return completed
            try:
                return process.wait(timeout=0)
            except subprocess.TimeoutExpired:
                return None
        try:
            return process.wait(timeout=min(remaining, 0.1))
        except subprocess.TimeoutExpired:
            continue


def run(seconds: float, marker: str, command: list[str]) -> int:
    pending_signal: List[Optional[int]] = [None]

    def handle_signal(signum: int, _frame: object) -> None:
        if pending_signal[0] is None:
            pending_signal[0] = signum

    old_handlers = {
        signum: signal.signal(signum, handle_signal)
        for signum in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        # start_new_session creates a new session and process group on both
        # supported hosts (stock macOS and Linux).  The group is what makes
        # timeout cleanup include compiler children and other descendants.
        process = subprocess.Popen(command, start_new_session=True)
    except FileNotFoundError:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
        return 127
    except PermissionError:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
        return 126
    except OSError as error:
        print("corpus-timeout: cannot start command: {}".format(error), file=sys.stderr)
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)
        return 1

    try:
        result = wait_for_process(process, seconds, pending_signal)
        if pending_signal[0] is not None:
            terminate_group(process, pending_signal[0])
            reap_after_termination(process)
            return 128 + pending_signal[0]
        if result is not None:
            return shell_status(result)

        terminate_group(process)
        reap_after_termination(process)
        write_timeout_marker(marker)
        return 124
    finally:
        for signum, handler in old_handlers.items():
            signal.signal(signum, handler)


def main(argv: list[str]) -> int:
    if len(argv) < 4:
        return usage()
    try:
        seconds = parse_seconds(argv[1])
    except ValueError as error:
        print("corpus-timeout: {}".format(error), file=sys.stderr)
        return 2
    return run(seconds, argv[2], argv[3:])


if __name__ == "__main__":
    sys.exit(main(sys.argv))
