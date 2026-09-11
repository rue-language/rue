#!/usr/bin/env python3
"""Record daemon resource use separately from fresh-process performance data."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import threading
import time


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("compiler", type=Path)
    parser.add_argument("repository", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--functions", type=int, default=4096)
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()
    if args.functions < 1 or args.repeats < 1:
        parser.error("functions and repeats must be positive")
    binary, repo, output = [path.resolve() for path in (args.compiler, args.repository, args.output)]
    if output.exists() and any(output.iterdir()):
        parser.error("output directory must be empty")
    output.mkdir(parents=True, exist_ok=True)
    fixtures = output / "fixtures"
    fixtures.mkdir()
    for name, source in [("startup", "performance/workloads/startup"), ("lattice", "performance/workloads/lattice")]:
        shutil.copytree(repo / source, fixtures / name)
    generator_path = repo / "performance/workloads/scale_functions/generate.py"
    spec = importlib.util.spec_from_file_location("function_fixture", generator_path)
    generator = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(generator)
    large = fixtures / "functions"
    large.mkdir()
    for name, source in generator.render(args.functions).items():
        (large / name).write_text(source)
    environment = dict(os.environ)
    # Tracing is deliberately unsupported by the opt-in service and would
    # change the resource workload if inherited from a developer shell.
    environment.pop("RUST_LOG", None)
    environment["RUE_DAEMON_ROOT"] = str(output / "endpoints")
    environment["RUE_STD_PATH"] = str(repo / "std")
    control_args = ["--scope", str(fixtures), "--isolation", "qualification"]
    build_args = ["--daemon-scope", str(fixtures), "--daemon-isolation", "qualification", "-j4"]
    metadata = {
        "compiler_sha256": sha(binary), "host": platform.platform(),
        "functions": args.functions, "repeats": args.repeats,
        "generator_sha256": sha(generator_path),
        "source_inputs": {str(p.relative_to(fixtures)): sha(p) for p in sorted(fixtures.rglob("*.rue"))},
        "std_inputs": {str(p.relative_to(repo / "std")): sha(p) for p in sorted((repo / "std").rglob("*.rue"))},
        "rss_note": "ps resident set in KiB sampled every 100 ms; process RSS includes allocator high-water memory and is not live query charge",
        "timing_note": "elapsed time is diagnostic context for resource calibration, not a benchmark-contract observation",
    }
    def run(arguments, label, directory=output, timeout=900):
        started = time.monotonic()
        result = subprocess.run([str(binary)] + arguments, cwd=directory, env=environment, capture_output=True, timeout=timeout)
        (output / (label + ".stdout")).write_bytes(result.stdout)
        (output / (label + ".stderr")).write_bytes(result.stderr)
        if result.returncode:
            raise RuntimeError("{} failed ({}): {}".format(label, result.returncode, result.stderr[-2500:].decode(errors="replace")))
        return result, time.monotonic() - started
    def status(label):
        result, _ = run(["daemon", "status"] + control_args + ["--json"], label, timeout=15)
        return json.loads(result.stdout)
    profile, _ = run(["--daemon=off", "--benchmark-json", "-j4", "main.rue", "-o", "profile.program"], "compiler-profile", fixtures / "startup")
    metadata["compiler_configuration"] = json.loads(profile.stdout)["compiler_boundary"]["configuration"]
    if metadata["compiler_configuration"]["compiler_build_profile"] != "release_thin_lto":
        raise RuntimeError("resource calibration requires a release_thin_lto compiler")
    (output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    rss_samples = []
    stop_sampling = threading.Event()
    monitor = None
    try:
        run(["daemon", "start"] + control_args + ["--idle-timeout-ms", "60000"], "start", timeout=30)
        initial = status("initial-status")
        pid = initial["service"]["pid"]
        def sample_rss():
            while not stop_sampling.is_set():
                observed = subprocess.run(["/bin/ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True)
                if observed.returncode == 0 and observed.stdout.strip():
                    rss_samples.append({"monotonic_s": time.monotonic(), "rss_kib": int(observed.stdout.strip())})
                stop_sampling.wait(0.1)
        monitor = threading.Thread(target=sample_rss)
        monitor.start()
        with (output / "requests.jsonl").open("w") as records:
            for rotation, name in enumerate(["startup", "lattice", "functions", "startup", "lattice", "startup"]):
                directory = fixtures / name
                for kind in ["build", "air"]:
                    mode_args = [] if kind == "build" else ["--emit", "air"]
                    direct_name = "r{}-{}-{}-direct".format(rotation, name, kind)
                    direct, _ = run(["--daemon=off", "-j4"] + mode_args + ["main.rue", "-o", "direct.program"], direct_name, directory)
                    direct_bytes = (directory / "direct.program").read_bytes() if kind == "build" else direct.stdout
                    for repeat in range(args.repeats):
                        label = "r{}-{}-{}-{}".format(rotation, name, kind, repeat)
                        before = len(rss_samples)
                        result, elapsed = run(["--daemon=required"] + build_args + mode_args + ["main.rue", "-o", "daemon.program"], label, directory)
                        actual = (directory / "daemon.program").read_bytes() if kind == "build" else result.stdout
                        if actual != direct_bytes or result.stderr != direct.stderr:
                            raise RuntimeError(label + ": direct/daemon output mismatch")
                        report = status(label + "-status")
                        samples = rss_samples[before:]
                        record = {
                            "rotation": rotation, "fixture": name, "kind": kind, "repeat": repeat,
                            "elapsed_s": elapsed, "output_bytes": len(actual),
                            "output_sha256": hashlib.sha256(actual).hexdigest(), "direct_equal": True,
                            "rss_peak_kib": max((sample["rss_kib"] for sample in samples), default=None),
                            "status": report,
                        }
                        records.write(json.dumps(record) + "\n")
                        records.flush()
                        print(json.dumps({key: record[key] for key in ["rotation", "fixture", "kind", "repeat", "elapsed_s", "rss_peak_kib"]}), flush=True)
        run(["daemon", "stop"] + control_args, "stop", timeout=40)
        # Stop acknowledges service shutdown before the OS necessarily reaps
        # the process. Observe that final boundary with a bounded wait.
        stopped_at = time.monotonic()
        while True:
            probe = subprocess.run(["/bin/ps", "-o", "pid=", "-p", str(pid)], capture_output=True, text=True)
            if probe.returncode == 1 and not probe.stdout.strip():
                break
            if probe.returncode != 0:
                raise RuntimeError("cannot observe stopped daemon: " + probe.stderr.strip())
            if time.monotonic() - stopped_at >= 5:
                raise RuntimeError("daemon process remains five seconds after stop")
            time.sleep(0.05)
        (output / "lifecycle.json").write_text(json.dumps({
            "pid": pid, "stopped_and_reaped": True,
            "reap_wait_s": time.monotonic() - stopped_at,
        }, indent=2) + "\n")
    finally:
        stop_sampling.set()
        if monitor is not None:
            monitor.join()
        (output / "rss.json").write_text(json.dumps(rss_samples) + "\n")
        subprocess.run([str(binary), "daemon", "stop"] + control_args, cwd=output, env=environment, capture_output=True, timeout=40)


if __name__ == "__main__":
    main()
