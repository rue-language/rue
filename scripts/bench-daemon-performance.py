#!/usr/bin/env python3
"""Qualify Rue's daemon with paired, externally timed real clients.

Rust owns record validity. This runner owns fixture transitions, subprocess
lifetimes, raw captures, and comparisons. Its clock starts before Popen and
ends after communicate/reap, including autostart, publication, and actual test
execution. Internal clocks are nested evidence. This is a separate regime
from fresh_source_to_native_v1.
"""

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time

SCENARIOS = [
    "first_client_empty", "prepared_no_edit", "mixed_analysis", "mixed_test",
    "mixed_build", "body_edit", "api_edit", "import_edit", "error", "repair",
    "revert", "eviction", "restart", "contention",
]


def digest(data):
    return hashlib.sha256(data).hexdigest()


def timestamp():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def inventory(root):
    return {str(path.relative_to(root)): digest(path.read_bytes())
            for path in sorted(root.rglob("*.rue"))}


def kill_session(session_id):
    """Kill only members of the session created for one timed-out invocation.

    Test children create their own process groups. Session membership still
    identifies them; the daemon deliberately detaches into a separate session
    and is stopped through its private control scope during final cleanup.
    """
    listed = subprocess.run(["/bin/ps", "-axo", "pid="], capture_output=True,
                            text=True, timeout=5, check=True)
    for item in listed.stdout.split():
        pid = int(item)
        try:
            if os.getsid(pid) == session_id:
                os.kill(pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass


class Qualification:
    def __init__(self, args):
        self.args = args
        self.output = args.output.resolve()
        if self.output.exists() and any(self.output.iterdir()):
            raise ValueError("output directory must be empty")
        self.output.mkdir(parents=True, exist_ok=True)
        self.raw = self.output / "raw"
        self.raw.mkdir()
        self.fixtures = self.output / "fixtures"
        self.fixtures.mkdir()
        self.image_sources = {"compiler": str(args.compiler), "validator": str(args.validator)}
        self.image_hashes = {}
        for name in ["compiler", "validator"]:
            source = getattr(args, name)
            destination = self.output / name
            expected = digest(source.read_bytes())
            shutil.copy2(source, destination)
            if digest(destination.read_bytes()) != expected:
                raise RuntimeError(name + " changed while its private image was copied")
            destination.chmod(0o555)
            self.image_hashes[name] = expected
            setattr(args, name, destination)
        self.env = dict(os.environ)
        self.env.pop("RUST_LOG", None)
        for name in list(self.env):
            if name.upper().startswith("MIMALLOC_"):
                del self.env[name]
        self.env["RUE_DAEMON_ROOT"] = str(self.output / "endpoints")
        self.env["RUE_STD_PATH"] = str(args.repository / "std")
        self.control = ["--scope", str(self.fixtures), "--isolation", "performance"]
        self.client = ["--daemon-scope", str(self.fixtures),
                       "--daemon-isolation", "performance"]
        self.observations = []
        self.evidence = {"processes": {}, "pairs": [], "lifetimes": [], "rss": []}
        self.pid = None
        self.canceling = threading.Event()
        self.children_lock = threading.Lock()
        self.children = {}
        self.sampling_done = threading.Event()
        self.monitor = threading.Thread(target=self.sample_rss, daemon=True)
        self.monitor.start()
        self.started_at = timestamp()

    def abort_active(self):
        self.canceling.set()
        with self.children_lock:
            children = list(self.children.values())
        for child in children:
            try:
                os.killpg(child.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 5
        while any(child.poll() is None for child in children) and time.monotonic() < deadline:
            time.sleep(0.05)
        # A parent may have exited while a test child still holds its pipes.
        # Include every owned session even when its original process exited.
        for child in children:
            kill_session(child.pid)

    def process(self, command, label, cwd=None, timeout=None, barrier=None, cleanup=False):
        if barrier is not None:
            barrier.wait(timeout=30)
        started = time.monotonic_ns()
        with self.children_lock:
            if self.canceling.is_set() and not cleanup:
                raise RuntimeError(label + ": qualification was interrupted")
            child = subprocess.Popen(command, cwd=str(cwd or self.output), env=self.env,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     start_new_session=True)
            self.children[child.pid] = child
        timed_out = False
        interrupted = False
        cleanup_error = None
        try:
            stdout, stderr = child.communicate(timeout=timeout or self.args.timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            try:
                os.killpg(child.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
            try:
                stdout, stderr = child.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                try:
                    kill_session(child.pid)
                except (OSError, subprocess.SubprocessError) as error:
                    cleanup_error = str(error)
                    child.kill()
                try:
                    stdout, stderr = child.communicate(timeout=2)
                except subprocess.TimeoutExpired as error:
                    stdout, stderr = error.stdout or b"", error.stderr or b""
                    child.stdout.close()
                    child.stderr.close()
                    child.wait(timeout=2)
        except KeyboardInterrupt:
            interrupted = True
            self.abort_active()
            stdout, stderr = child.communicate(timeout=2)
        finished = time.monotonic_ns()
        with self.children_lock:
            self.children.pop(child.pid, None)
        (self.raw / (label + ".stdout")).write_bytes(stdout)
        (self.raw / (label + ".stderr")).write_bytes(stderr)
        record = {"command": [str(arg) for arg in command], "cwd": str(cwd or self.output),
                  "pid": child.pid, "started_ns": started, "finished_ns": finished,
                  "elapsed_ns": finished - started, "exit_code": child.returncode,
                  "timed_out": timed_out,
                  "interrupted": interrupted,
                  "cleanup_error": cleanup_error,
                  "stdout_sha256": digest(stdout), "stderr_sha256": digest(stderr)}
        self.evidence["processes"][label] = record
        if interrupted:
            raise RuntimeError(label + ": qualification was interrupted; run is incomplete")
        if timed_out:
            raise RuntimeError(label + ": subprocess timed out; run is incomplete")
        if child.returncode < 0:
            raise RuntimeError("{}: subprocess died from signal {}".format(label, -child.returncode))
        return record, stdout, stderr

    def check(self, action, source, label):
        record, stdout, stderr = self.process(
            [str(self.args.validator), "daemon-performance", action, "--input", str(source)],
            label, timeout=30)
        if record["exit_code"]:
            raise RuntimeError("{}: Rust validation failed: {}".format(
                label, stderr.decode(errors="replace")[-4000:]))
        return stdout

    def status(self, label):
        record, stdout, stderr = self.process(
            [str(self.args.compiler), "daemon", "status"] + self.control + ["--json"],
            label, timeout=30, cleanup=label.startswith("cleanup-"))
        if record["exit_code"]:
            raise RuntimeError(label + ": daemon status failed: " + stderr.decode(errors="replace"))
        value = json.loads(stdout)
        self.pid = value["service"]["pid"]
        return value

    def stop(self, label):
        if self.pid is None:
            return
        pid = self.pid
        record, _, stderr = self.process(
            [str(self.args.compiler), "daemon", "stop"] + self.control, label, timeout=40,
            cleanup=True)
        if record["exit_code"]:
            raise RuntimeError(label + ": stop failed: " + stderr.decode(errors="replace"))
        deadline = time.monotonic() + 5
        while True:
            probe = subprocess.run(["/bin/ps", "-o", "pid=", "-p", str(pid)],
                                   capture_output=True, text=True, timeout=5)
            if probe.returncode == 1 and not probe.stdout.strip():
                break
            if probe.returncode != 0 or time.monotonic() >= deadline:
                raise RuntimeError(label + ": service was not observed reaped after stop")
            time.sleep(0.05)
        self.evidence["lifetimes"].append({"pid": pid, "stopped_and_reaped": True,
                                           "stopped_at": timestamp()})
        self.pid = None

    def sample_rss(self):
        while not self.sampling_done.wait(0.1):
            pid = self.pid
            if pid is None:
                continue
            try:
                result = subprocess.run(["/bin/ps", "-o", "rss=", "-p", str(pid)],
                                        capture_output=True, text=True, timeout=5)
                if result.returncode == 0 and result.stdout.strip():
                    self.evidence["rss"].append({"pid": pid, "monotonic_ns": time.monotonic_ns(),
                                                 "rss_kib": int(result.stdout.strip())})
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                self.evidence["rss"].append({"pid": pid, "error": str(error)})

    def fixtures_setup(self):
        revision, stdout, _ = self.process(["git", "rev-parse", "HEAD"], "source-revision",
                                           self.args.repository, timeout=30)
        status, changed, _ = self.process(["git", "status", "--porcelain"], "source-status",
                                          self.args.repository, timeout=30)
        if revision["exit_code"] or stdout.decode().strip() != self.args.revision:
            raise RuntimeError("repository HEAD does not match --revision")
        if status["exit_code"] or changed.strip():
            raise RuntimeError("release qualification requires a clean source checkout")
        for name in ["startup", "lattice"]:
            shutil.copytree(self.args.repository / "performance" / "workloads" / name,
                            self.fixtures / name)
        generator_path = self.args.repository / "performance/workloads/scale_functions/generate.py"
        spec = importlib.util.spec_from_file_location("daemon_functions_fixture", generator_path)
        generator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(generator)
        large = self.fixtures / "functions"
        large.mkdir()
        for name, source in generator.render(self.args.functions).items():
            (large / name).write_text(source, encoding="utf-8")
        self.edit("baseline")
        profile, stdout, stderr = self.process(
            [str(self.args.compiler), "--daemon=off", "--benchmark-json", "-j4", "main.rue",
             "-o", str(self.output / "profile.program")],
            "compiler-profile", self.fixtures / "startup")
        if profile["exit_code"]:
            raise RuntimeError("cannot establish compiler profile: " + stderr.decode(errors="replace"))
        configuration = json.loads(stdout)["compiler_boundary"]["configuration"]
        if configuration["compiler_build_profile"] != "release_thin_lto":
            raise RuntimeError("qualification requires a release_thin_lto compiler")
        host_details = {"logical_cpus": os.cpu_count(), "cpu_model": None, "memory_bytes": None}
        if platform.system() == "Darwin":
            host, stdout, stderr = self.process(
                ["sysctl", "-n", "machdep.cpu.brand_string", "hw.memsize", "hw.logicalcpu"],
                "host-details", timeout=30)
            if host["exit_code"]:
                raise RuntimeError("cannot establish host details: " + stderr.decode(errors="replace"))
            cpu, memory, logical = stdout.decode().splitlines()
            host_details = {"logical_cpus": int(logical), "cpu_model": cpu, "memory_bytes": int(memory)}
        self.evidence["metadata"] = {
            "compiler_revision": self.args.revision,
            "compiler_image_sha256": self.image_hashes["compiler"],
            "validator_image_sha256": self.image_hashes["validator"],
            "image_source_paths": self.image_sources,
            "image_policy": "private read-only copies hashed before and after the run; hashes are external evidence and their cost is outside client clocks",
            "compiler_configuration": configuration,
            "host": platform.platform(), "python": platform.python_version(),
            "host_details": host_details,
            "functions": self.args.functions, "repeats": self.args.repeats,
            "generator_sha256": digest(generator_path.read_bytes()),
            "initial_source_inputs": inventory(self.fixtures),
            "std_inputs": inventory(self.args.repository / "std"),
            "timing_boundary": "monotonic_ns before Popen through communicate and child reap",
            "cold_policy": "empty private endpoint; direct reference first may warm OS file cache",
            "worker_policy": "build/analysis: four compiler workers; test: one test process and automatic compiler workers; resolved counts are in endpoint identity",
            "environment_policy": "remove RUST_LOG and every case-insensitive MIMALLOC_ variable; explicit private RUE_DAEMON_ROOT and repository RUE_STD_PATH",
            "rss_policy": "ps RSS KiB at 100 ms after first reply identifies PID; cold-start peak unavailable; RSS includes allocator high-water memory",
            "test_policy": "real test execution, JSON events, seed 1, one test process; Rust projects verdicts/captures without event clocks",
        }

    def edit(self, revision):
        self.fixture_expected_exit = 43 if revision == "body" else 42
        directory = self.fixtures / "edit"
        directory.mkdir(exist_ok=True)
        module = "helper2.rue" if revision == "import" else "helper.rue"
        call = "helper.value(42)" if revision == "api" else "helper.value()"
        source = ('const helper = @import("' + module + '");\n'
                  'fn main() -> i32 { ' + call + ' }\n'
                  'test "helper stays positive" { @assert(' + call + ' > 0); }\n')
        helper = "pub fn value() -> i32 { 42 }\n"
        if revision == "body":
            helper = "pub fn value() -> i32 { 43 }\n"
        elif revision == "api":
            helper = "pub fn value(input: i32) -> i32 { input }\n"
        elif revision == "error":
            helper = "pub fn value() -> i32 { missing_name }\n"
        (directory / "main.rue").write_text(source, encoding="utf-8")
        (directory / "helper.rue").write_text(helper, encoding="utf-8")
        alternate = directory / "helper2.rue"
        if revision == "import":
            alternate.write_text(helper, encoding="utf-8")
        elif alternate.exists():
            alternate.unlink()

    def invoke(self, sequence, path, artifact, fixture, scenario, barrier=None):
        label = "{:03d}-{}".format(sequence, path)
        directory = self.fixtures / fixture
        sidecar = self.raw / (label + ".sidecar.json")
        program = self.raw / (label + ".program")
        flags = ["--daemon=" + ("off" if path == "direct_fresh" else "required")]
        if path == "daemon":
            flags += self.client
        flags += ["--daemon-performance-json", str(sidecar)]
        if artifact == "test_image":
            args = ["test"] + flags + ["--format", "json", "--seed", "1", "--jobs", "1", "main.rue"]
        else:
            args = flags + ["-j4", "main.rue"]
            args += ["--emit", "air"] if artifact == "analysis" else ["-o", str(program)]
        result, stdout, stderr = self.process([str(self.args.compiler)] + args, label,
                                              directory, barrier=barrier)
        expected = 1 if scenario == "error" else 0
        if result["exit_code"] != expected:
            raise RuntimeError("{}: expected compiler exit {}, got {}: {}".format(
                label, expected, result["exit_code"], stderr.decode(errors="replace")[-4000:]))
        # Preserve the raw sidecar. Only the external observer can fill the
        # client lifetime, published output, and captured diagnostic fields.
        self.check("validate", sidecar, label + "-sidecar-validation")
        sidecar_record = json.loads(sidecar.read_text(encoding="utf-8"))
        if sidecar_record["artifact"] != artifact:
            raise RuntimeError(label + ": compiler observed a different artifact than requested")
        endpoint = sidecar_record["endpoint"]
        endpoint["identity"]["compiler_image_sha256"] = self.image_hashes["compiler"]
        endpoint["timing"]["client_started_ns"] = result["started_ns"]
        endpoint["timing"]["client_spawn_to_exit_ns"] = result["elapsed_ns"]
        endpoint["diagnostics_sha256"] = digest(stderr)
        endpoint["exit_code"] = result["exit_code"]
        if artifact == "executable" and expected == 0:
            endpoint["output_sha256"] = digest(program.read_bytes())
            program_args = [str(program)] + (["selftest"] if fixture == "lattice" else [])
            behavior, _, _ = self.process(program_args, label + "-execution", directory, timeout=60)
            expected_program = self.fixture_expected_exit if fixture == "edit" else 0
            if behavior["exit_code"] != expected_program:
                raise RuntimeError(label + ": produced program failed its expected behavior")
            canonical = {key: behavior[key] for key in ["exit_code", "stdout_sha256", "stderr_sha256"]}
            endpoint["behavior_sha256"] = digest(json.dumps(canonical, sort_keys=True).encode())
        elif artifact == "test_image":
            projection = json.loads(self.check("test-output", self.raw / (label + ".stdout"),
                                               label + "-test-projection"))
            endpoint["output_sha256"] = endpoint["prepared_image_sha256"]
            endpoint["behavior_sha256"] = projection["output_sha256"]
            endpoint["execution_proof_sha256"] = digest(stdout)
            count = projection["tests_executed_count"]
            if count < 1 or count != endpoint["tests_executed_count"]:
                raise RuntimeError(label + ": reaped tests disagree with the event stream")
        else:
            endpoint["output_sha256"] = digest(stdout)
        return endpoint, sidecar_record.get("work"), result

    def append_pair(self, scenario, artifact, fixture, direct, daemon, status, extra=None):
        sequence = len(self.observations)
        d_endpoint, _, d_process = direct
        s_endpoint, work, s_process = daemon
        equal = all(d_endpoint.get(key) == s_endpoint.get(key) for key in
                    ["output_sha256", "diagnostics_sha256", "exit_code", "behavior_sha256"])
        if not equal:
            raise RuntimeError("{} {} {}: direct/daemon behavior differs".format(sequence, scenario, fixture))
        if work is None:
            raise RuntimeError("request-correlated daemon work measurement is absent")
        self.observations.append({"sequence": sequence, "scenario": scenario, "artifact": artifact,
                                  "direct": d_endpoint, "daemon": s_endpoint,
                                  "work": work, "direct_equivalent": equal,
                                  "contention": None if extra is None else {
                                      "group": extra["contention_group"], "clients": extra["clients"],
                                      "observed_queued_requests": max(
                                          item["queued_requests"] for item in extra["queue_observations"]),
                                  }})
        # Status is global resource evidence, never a substitute for the
        # ticket-correlated measurement returned with this request's answer.
        resources = dict(status)
        resources.pop("last_measurement", None)
        self.evidence["pairs"].append({"sequence": sequence, "fixture": fixture,
                                       "fixture_inputs": inventory(self.fixtures / fixture),
                                       "status_after": resources, "extra": extra})
        self.persist(False)
        print(json.dumps({"sequence": sequence, "scenario": scenario, "fixture": fixture,
                          "artifact": artifact, "direct_ms": d_process["elapsed_ns"] / 1e6,
                          "daemon_ms": s_process["elapsed_ns"] / 1e6}), flush=True)

    def pair(self, scenario, artifact="executable", fixture="edit"):
        sequence = len(self.observations)
        direct = self.invoke(sequence, "direct_fresh", artifact, fixture, scenario)
        daemon = self.invoke(sequence, "daemon", artifact, fixture, scenario)
        status = self.status("{:03d}-status".format(sequence))
        self.append_pair(scenario, artifact, fixture, direct, daemon, status)
        return status

    def contention(self):
        first = len(self.observations)
        results = {}
        queue_evidence = []
        for path in ["direct_fresh", "daemon"]:
            barrier = threading.Barrier(2)
            with ThreadPoolExecutor(max_workers=2) as pool:
                try:
                    jobs = [pool.submit(self.invoke, first + index, path, "executable", "lattice",
                                        "contention", barrier) for index in range(2)]
                    if path == "daemon":
                        while not all(job.done() for job in jobs):
                            observed = self.status("{:03d}-queue-{:03d}".format(first, len(queue_evidence)))
                            observed.pop("last_measurement", None)
                            queue_evidence.append(observed)
                            if observed["queued_requests"] > 0:
                                break
                    results[path] = [job.result() for job in jobs]
                except BaseException:
                    # ThreadPoolExecutor waits for workers on scope exit.
                    # Cancel their sessions before that wait, including when
                    # Ctrl-C interrupted a main-thread status/future wait.
                    self.abort_active()
                    raise
            intervals = [result[2] for result in results[path]]
            if max(item["started_ns"] for item in intervals) >= min(item["finished_ns"] for item in intervals):
                raise RuntimeError(path + ": contention clients did not overlap")
        if not any(observed["queued_requests"] > 0 for observed in queue_evidence):
            raise RuntimeError("contention did not observe a queued daemon request")
        status = self.status("{:03d}-contention-status".format(first))
        for index in range(2):
            self.append_pair("contention", "executable", "lattice",
                             results["direct_fresh"][index], results["daemon"][index], status,
                             {"contention_group": first, "clients": 2,
                              "queue_observations": queue_evidence})

    def persist(self, complete):
        report = {
            "record_kind": "daemon_client_to_publication_v1", "schema_version": 1,
            "compiler_revision": self.args.revision,
            "compiler_image_sha256": self.image_hashes["compiler"],
            "started_at": self.started_at, "finished_at": timestamp(),
            "required_scenarios": SCENARIOS, "qualification_complete": complete,
            "observations": self.observations,
        }
        write_json(self.output / "report.json", report)
        write_json(self.output / "evidence.json", self.evidence)

    def summarize(self):
        groups = {}
        for observation, evidence in zip(self.observations, self.evidence["pairs"]):
            key = (observation["scenario"], evidence["fixture"], observation["artifact"])
            groups.setdefault(key, []).append(observation)
        rows = []
        for (scenario, fixture, artifact), observations in sorted(groups.items()):
            direct = [item["direct"]["timing"]["client_spawn_to_exit_ns"] / 1e6
                      for item in observations]
            daemon = [item["daemon"]["timing"]["client_spawn_to_exit_ns"] / 1e6
                      for item in observations]
            rows.append({"scenario": scenario, "fixture": fixture, "artifact": artifact,
                         "pairs": len(observations),
                         "direct_ms": {"min": min(direct), "median": statistics.median(direct),
                                       "max": max(direct)},
                         "daemon_ms": {"min": min(daemon), "median": statistics.median(daemon),
                                       "max": max(daemon)},
                         "median_paired_direct_over_daemon": statistics.median(
                             [left / right for left, right in zip(direct, daemon)])})
        peaks = {}
        for sample in self.evidence["rss"]:
            if "rss_kib" in sample:
                pid = str(sample["pid"])
                peaks[pid] = max(peaks.get(pid, 0), sample["rss_kib"])
        write_json(self.output / "summary.json", {
            "note": "Descriptive observations on this host, without confidence intervals; ratios above one favor the daemon. RSS sampling misses initial startup and is distinct from live query charge.",
            "latency": rows, "sampled_lifetime_peak_rss_kib": peaks,
            "max_observed_retained_query_charge_bytes": max(
                pair["status_after"]["resource_pressure"]["peak_retained_charge_bytes"]
                for pair in self.evidence["pairs"]),
            "max_observed_response_charge_bytes": max(
                pair["status_after"]["resource_pressure"]["peak_response_bytes"]
                for pair in self.evidence["pairs"]),
        })

    def run(self):
        self.fixtures_setup()
        # No daemon start/status command precedes the first measured client.
        self.pair("first_client_empty", fixture="startup")
        for _ in range(self.args.repeats):
            self.pair("prepared_no_edit", fixture="startup")
        # Rotate realistic root sets, then exercise both request-kind orders
        # within one root. Test preparation cannot substitute for test runs.
        for _ in range(self.args.repeats):
            self.pair("mixed_build", fixture="lattice")
            self.pair("prepared_no_edit", fixture="lattice")
            self.pair("mixed_analysis", "analysis", "lattice")
        for artifact, scenario in [
            ("analysis", "mixed_analysis"), ("test_image", "mixed_test"),
            ("executable", "mixed_build"), ("test_image", "mixed_test"),
            ("analysis", "mixed_analysis"), ("executable", "mixed_build"),
        ]:
            self.pair(scenario, artifact)
        for _ in range(self.args.repeats):
            self.pair("prepared_no_edit", "test_image")
        for revision, scenario in [
            ("body", "body_edit"), ("api", "api_edit"), ("import", "import_edit"),
            ("error", "error"), ("body", "repair"), ("baseline", "revert"),
        ]:
            self.edit(revision)
            self.pair(scenario)
        # Repeating the same over-budget root must reopen a session; neither
        # labeling a rotation eviction nor observing RSS alone proves that.
        generations = []
        for _ in range(2):
            status = self.pair("eviction", fixture="functions")
            if status["retained_hosts"] != 0:
                raise RuntimeError("large fixture did not force host eviction; increase --functions")
            generations.append(self.observations[-1]["daemon"]["identity"]["session_generation"])
        if generations[0] == generations[1]:
            raise RuntimeError("evicted host did not acquire a new session generation")
        self.pair("mixed_build")
        before = self.observations[-1]["daemon"]["identity"]["daemon_generation"]
        self.stop("restart-stop")
        self.pair("restart")
        after = self.observations[-1]["daemon"]["identity"]["daemon_generation"]
        if before == after:
            raise RuntimeError("restart did not create a new daemon generation")
        self.contention()
        self.stop("final-stop")
        for name in ["compiler", "validator"]:
            if digest(getattr(self.args, name).read_bytes()) != self.image_hashes[name]:
                raise RuntimeError(name + " image changed during qualification")
        self.persist(True)
        self.check("validate", self.output / "report.json", "report-validation")
        self.summarize()

    def close(self):
        try:
            # A failed first client may have started the private service before
            # the runner learned its PID. Discover it only in this fresh scope.
            if self.pid is None and (self.output / "endpoints").exists():
                try:
                    self.status("cleanup-status")
                except RuntimeError:
                    pass
            self.stop("cleanup-stop")
        finally:
            self.sampling_done.set()
            self.monitor.join(timeout=6)
            write_json(self.output / "evidence.json", self.evidence)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--compiler", type=Path, required=True)
    parser.add_argument("--validator", type=Path, required=True)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--revision", required=True, help="clean source commit used for this compiler image")
    parser.add_argument("--functions", type=int, default=4096)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=900)
    args = parser.parse_args()
    if len(args.revision) != 40 or any(c not in "0123456789abcdef" for c in args.revision):
        parser.error("revision must be a lowercase 40-character Git commit")
    if args.functions < 1 or args.repeats < 1 or args.timeout <= 0:
        parser.error("functions, repeats, and timeout must be positive")
    for name in ["compiler", "validator", "repository"]:
        setattr(args, name, getattr(args, name).resolve(strict=True))
    runner = None
    failed = None
    try:
        runner = Qualification(args)
        runner.run()
    except KeyboardInterrupt:
        failed = "qualification was interrupted; run is incomplete"
        if runner is not None:
            runner.abort_active()
            runner.evidence["failure"] = failed
            runner.persist(False)
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        failed = str(error)
        if runner is not None:
            runner.evidence["failure"] = failed
            runner.persist(False)
    finally:
        if runner is not None:
            try:
                runner.close()
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                failed = "{}; cleanup: {}".format(failed or "qualification", error)
                runner.evidence["failure"] = failed
                runner.persist(False)
    if failed:
        print("daemon qualification: " + failed, file=sys.stderr)
        return 2
    print("Validated paired report: " + str(args.output.resolve() / "report.json"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
