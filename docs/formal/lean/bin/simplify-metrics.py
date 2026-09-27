#!/usr/bin/env python3
"""The proof-simplification metrics of `SIMPLIFY.md` (RUE-2471), as a markdown report.

Run from `docs/formal/lean` (or pass `--lean-dir`):

    python3 bin/simplify-metrics.py              # everything, build times included
    python3 bin/simplify-metrics.py --no-time    # the deterministic metrics only

It builds the package (`lake build RueCore RueCore.Map`, a no-op on a warm tree), then measures:

* lines per module: total lines, and code lines (neither blank nor inside a comment,
  doc-comments included), from the sources;
* theorems per module, helper lemmas per spine theorem, and duplicated lemma shapes, from the
  compiled environment, by `bin/simplify_metrics.lean` run with `lake env lean --run`
  (its module docstring says exactly what each count means);
* build time per module: `lake env lean <file>` with the module's imports already built, run
  `--runs` times per module in turn; the median wall time, the spread, and the median CPU time
  (user plus system, of the child processes). Noisy: see `SIMPLIFY.md`, "Metric methods";
* `set_option` overrides (`maxHeartbeats` and every other option), from the sources outside
  comments and string literals, with their file and line;
* build warnings per module, from `lake build`'s output, which replays each module's cached
  log on a warm tree: unused `simp` arguments counted apart from the rest.

Everything but the build times is deterministic: two runs on the same tree print the same
report, less the timing column (`--no-time` omits it). L3 tooling only: nothing here is a
module of the package, and nothing the layering audit or the trusted-base lint walks imports it.
"""

import argparse
import json
import os
import re
import resource
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
HELPER = os.path.join(HERE, "simplify_metrics.lean")


class ScriptError(Exception):
    pass


def run(cmd, cwd, check=True):
    p = subprocess.run(cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                       universal_newlines=True)
    if check and p.returncode != 0:
        raise ScriptError("{} failed ({}):\n{}{}".format(
            " ".join(cmd), p.returncode, p.stdout[-2000:], p.stderr[-2000:]))
    return p


# -- sources -------------------------------------------------------------------------------

def modules(lean_dir):
    """Every source module of the package: (module name, path relative to lean_dir), sorted."""
    out = [("RueCore", "RueCore.lean")]
    for root, _, files in os.walk(os.path.join(lean_dir, "RueCore")):
        for f in files:
            if f.endswith(".lean"):
                rel = os.path.relpath(os.path.join(root, f), lean_dir)
                out.append((rel[:-5].replace(os.sep, "."), rel))
    return sorted(out)


def layers(lean_dir):
    """The layer table of `RueCore/Layers.lean`: module name to layer label."""
    names = {0: "L0", 1: "L1", 2: "Spec", 3: "L2", 4: "L3"}
    src = open(os.path.join(lean_dir, "RueCore", "Layers.lean"), encoding="utf-8").read()
    return {m: names[int(n)] for m, n in re.findall(r"\(`([\w.]+), (\d)\)", src)}


CHAR_LIT = re.compile(r"'(\\.|[^\\'\n])'")


def strip_comments(src):
    """The source with every comment and string literal blanked (newlines kept): `--` line
    comments, nested `/- … -/` block comments (doc-comments too), `"…"` strings and char
    literals. A `'` that is part of an identifier (`H'`) is left alone."""
    out = []
    i, n, depth = 0, len(src), 0
    while i < n:
        c = src[i]
        if depth:
            if src.startswith("/-", i):
                depth += 1
                out.append("  ")
                i += 2
            elif src.startswith("-/", i):
                depth -= 1
                out.append("  ")
                i += 2
            else:
                out.append("\n" if c == "\n" else " ")
                i += 1
        elif src.startswith("/-", i):
            depth = 1
            out.append("  ")
            i += 2
        elif src.startswith("--", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            seg = src[i:j + 1]
            out.append(re.sub(r"[^\n]", " ", seg))
            i = j + 1
        elif c == "'" and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] in "_'.!?")):
            m = CHAR_LIT.match(src, i)
            if m:
                out.append(" " * len(m.group(0)))
                i = m.end()
            else:
                out.append(c)
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


SET_OPTION = re.compile(r"\bset_option\s+([\w.«»]+)\s+(\S+)")


def scan_sources(lean_dir, mods):
    """Per module: total lines, code lines, and the `set_option` uses as (line, name, value)."""
    out = {}
    for mod, rel in mods:
        src = open(os.path.join(lean_dir, rel), encoding="utf-8").read()
        code = strip_comments(src)
        lines = src.split("\n")
        if lines and lines[-1] == "":
            lines.pop()
        code_lines = sum(1 for ln in code.split("\n") if ln.strip())
        opts = []
        for m in SET_OPTION.finditer(code):
            line = code.count("\n", 0, m.start()) + 1
            opts.append((line, m.group(1), m.group(2)))
        out[mod] = {"lines": len(lines), "code": code_lines, "options": opts, "path": rel}
    return out


# -- the build and its warnings ------------------------------------------------------------

WARNING = re.compile(r"^warning: ([\w/.]+\.lean):(\d+):(\d+): (.*)$")


def build(lean_dir, log=None):
    """Build the package and the proof map's module; return the per-module warnings as
    {module: {"simp": n, "other": n, "kinds": {first line: n}}}."""
    if log:
        text = open(log, encoding="utf-8").read()
    else:
        p = run(["lake", "build", "RueCore", "RueCore.Map"], lean_dir, check=False)
        text = p.stdout + p.stderr
        if p.returncode != 0:
            raise ScriptError("lake build failed:\n" + text[-3000:])
    out = {}
    for ln in text.split("\n"):
        m = WARNING.match(ln.strip())
        if not m:
            continue
        mod = m.group(1)[:-5].replace("/", ".")
        rec = out.setdefault(mod, {"simp": 0, "other": 0, "kinds": {}, "simpLines": []})
        msg = m.group(4).strip()
        if msg.startswith("This simp argument is unused"):
            rec["simp"] += 1
            rec["simpLines"].append(int(m.group(2)))
        else:
            rec["other"] += 1
            rec["kinds"][msg] = rec["kinds"].get(msg, 0) + 1
    return out


def env_metrics(lean_dir):
    p = run(["lake", "env", "lean", "--run", HELPER], lean_dir, check=False)
    if p.returncode != 0:
        raise ScriptError("simplify_metrics.lean failed (is the package built?):\n"
                          + p.stdout[-2000:] + p.stderr[-2000:])
    return json.loads(p.stdout)


def child_cpu():
    r = resource.getrusage(resource.RUSAGE_CHILDREN)
    return r.ru_utime + r.ru_stime


def build_times(lean_dir, mods, runs):
    """Per module: (median wall s, wall spread s, median CPU s) of `lake env lean <file>`."""
    out = {}
    for mod, rel in mods:
        walls, cpus = [], []
        for _ in range(runs):
            c0, t0 = child_cpu(), time.monotonic()
            run(["lake", "env", "lean", rel], lean_dir)
            walls.append(time.monotonic() - t0)
            cpus.append(child_cpu() - c0)
        out[mod] = (statistics.median(walls), max(walls) - min(walls), statistics.median(cpus))
    return out


# -- the report ----------------------------------------------------------------------------

def short(name):
    return name[len("RueCore."):] if name.startswith("RueCore.") else name


def is_glue(mods):
    return all(m.endswith(".Glue") for m in mods)


def report(lean_dir, args):
    mods = modules(lean_dir)
    layer = layers(lean_dir)
    warnings = build(lean_dir, args.build_log)
    env = env_metrics(lean_dir)
    src = scan_sources(lean_dir, mods)
    times = build_times(lean_dir, mods, args.runs) if not args.no_time else None

    thm_module = {t["name"]: t["module"] for t in env["theorems"]}
    thm_lines = {t["name"]: t["lines"] for t in env["theorems"]}
    per_mod = {}
    for t in env["theorems"]:
        per_mod[t["module"]] = per_mod.get(t["module"], 0) + 1

    sha = run(["git", "rev-parse", "--short=9", "HEAD"], lean_dir).stdout.strip()
    dirty = run(["git", "status", "--porcelain", "--", "."], lean_dir).stdout.strip()
    toolchain = open(os.path.join(lean_dir, "lean-toolchain")).read().strip()
    o = []
    o.append("# Proof-simplification metrics")
    o.append("")
    o.append("Generated by `python3 bin/simplify-metrics.py{}` at `{}`{}, toolchain `{}`.".format(
        " --no-time" if args.no_time else "", sha, " (with local changes)" if dirty else "",
        toolchain))
    o.append("")

    # modules
    o.append("## Lines, theorems, build time, overrides and warnings per module")
    o.append("")
    head = "| Module | Layer | Lines | Code lines | Theorems |"
    rule = "| --- | --- | ---: | ---: | ---: |"
    if times:
        head += " Build wall s (median of {}) | Spread s | CPU s |".format(args.runs)
        rule += " ---: | ---: | ---: |"
    head += " `set_option` | Unused `simp` args | Other warnings |"
    rule += " ---: | ---: | ---: |"
    o += [head, rule]
    tot = {"lines": 0, "code": 0, "thm": 0, "wall": 0.0, "cpu": 0.0, "opt": 0, "simp": 0, "other": 0}
    order = sorted(mods, key=lambda mr: (layer.get(mr[0], "L9"), mr[0]))
    for mod, _ in order:
        s = src[mod]
        w = warnings.get(mod, {"simp": 0, "other": 0})
        row = "| `{}` | {} | {} | {} | {} |".format(
            short(mod) or mod, layer.get(mod, "?"), s["lines"], s["code"], per_mod.get(mod, 0))
        if times:
            wall, spread, cpu = times[mod]
            row += " {:.2f} | {:.2f} | {:.2f} |".format(wall, spread, cpu)
            tot["wall"] += wall
            tot["cpu"] += cpu
        row += " {} | {} | {} |".format(len(s["options"]), w["simp"], w["other"])
        o.append(row)
        tot["lines"] += s["lines"]
        tot["code"] += s["code"]
        tot["thm"] += per_mod.get(mod, 0)
        tot["opt"] += len(s["options"])
        tot["simp"] += w["simp"]
        tot["other"] += w["other"]
    row = "| all {} modules | | {} | {} | {} |".format(len(mods), tot["lines"], tot["code"], tot["thm"])
    if times:
        row += " {:.1f} | | {:.1f} |".format(tot["wall"], tot["cpu"])
    row += " {} | {} | {} |".format(tot["opt"], tot["simp"], tot["other"])
    o.append(row)
    o.append("")
    by_layer = {}
    for mod, _ in mods:
        rec = by_layer.setdefault(layer.get(mod, "?"), [0, 0, 0])
        rec[0] += src[mod]["lines"]
        rec[1] += src[mod]["code"]
        rec[2] += per_mod.get(mod, 0)
    o.append("By layer: " + "; ".join("{} {} lines ({} code), {} theorems".format(k, *v)
                                      for k, v in sorted(by_layer.items())) + ".")
    o.append("")

    o.append("### `set_option` overrides")
    o.append("")
    any_opt = False
    for mod, _ in order:
        for line, name, val in src[mod]["options"]:
            any_opt = True
            o.append("* `{}:{}`: `set_option {} {}` ({})".format(
                src[mod]["path"], line, name, val, layer.get(mod, "?")))
    if not any_opt:
        o.append("None.")
    o.append("")

    o.append("### Build warnings")
    o.append("")
    any_w = False
    for mod, _ in order:
        w = warnings.get(mod)
        if not w:
            continue
        any_w = True
        parts = []
        if w["simp"]:
            parts.append("{} unused `simp` arguments (lines {})".format(
                w["simp"], ", ".join(str(x) for x in w["simpLines"])))
        for k, v in sorted(w["kinds"].items()):
            parts.append("{} × {}".format(v, k))
        o.append("* `{}` ({}): {}".format(src[mod]["path"], layer.get(mod, "?"), "; ".join(parts)))
    if not any_w:
        o.append("None.")
    o.append("")

    # spine
    o.append("## Helper lemmas per spine theorem")
    o.append("")
    o.append("Authored non-spine theorems a spine theorem's proof depends on transitively "
             "(`bin/simplify_metrics.lean`). All: walking through other spine theorems too. "
             "Own: stopping at another spine theorem. MAP: `MAP.md`'s size-stats count, which "
             "stops at every marked node and counts generated lemmas too.")
    o.append("")
    o.append("| Spine theorem | Module | Proof lines | Helpers (all) | Their lines | "
             "Helpers (own) | Spine theorems used | MAP |")
    o.append("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |")
    for s in env["spine"]:
        o.append("| `{}` | `{}` | {} | {} | {} | {} | {} | {} |".format(
            short(s["name"]), short(s["module"]), s["lines"], s["all"], s["allLines"],
            s["own"], s["spineMet"], s["map"]))
    o.append("")

    # duplicate shapes
    def render_groups(groups, skip=None):
        rows = []
        glue = []
        for g in groups:
            names = g["names"]
            mods_ = [thm_module[n] for n in names]
            if skip and skip(g):
                continue
            if is_glue(mods_):
                glue.append(g)
                continue
            rows.append("* {} (type size {}): {}".format(
                len(names), g["size"],
                ", ".join("`{}` ({}, {} lines)".format(short(n), short(thm_module[n]),
                                                       thm_lines[n]) for n in names)))
        if not rows:
            rows.append("* none")
        if glue:
            n = sum(len(g["names"]) for g in glue)
            rows.append("* and {} groups ({} theorems) entirely inside the witness glue "
                        "modules (`Nonvacuous.Glue`, `Sharp.Glue`), where one statement is "
                        "re-proved per witness by design: sizes {}".format(
                            len(glue), n, ", ".join(str(len(g["names"])) for g in glue)))
        return rows

    o.append("## Duplicated lemma shapes")
    o.append("")
    o.append("Exact: the same statement up to α-equivalence (universe and binder names, binder "
             "annotations and metadata erased).")
    o.append("")
    o += render_groups(env["exact"])
    o.append("")
    exact_sets = {tuple(g["names"]) for g in env["exact"]}
    o.append("Skeleton, beyond the exact groups: the same statement once every predicate of the "
             "package is replaced by one placeholder. A group whose statement is a single "
             "predicate (type size 1: `Spine`'s re-statements of the Spec layer) is left out.")
    o.append("")
    o += render_groups(env["skeleton"],
                       skip=lambda g: tuple(g["names"]) in exact_sets or g["size"] <= 1)
    o.append("")
    return "\n".join(o)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--lean-dir", default=os.path.dirname(HERE))
    ap.add_argument("--no-time", action="store_true", help="skip the build-time measurement")
    ap.add_argument("--runs", type=int, default=3, help="timed runs per module (default 3)")
    ap.add_argument("--build-log", help="read warnings from this `lake build` log instead")
    args = ap.parse_args()
    try:
        print(report(os.path.abspath(args.lean_dir), args))
    except ScriptError as e:
        print("simplify-metrics: " + str(e), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
