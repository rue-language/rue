"""Classify Rust std's public API against the proposed Rue borrow rule.

The rule under test:
  R1  Inside a function body, borrows are first-class: locals may hold values
      whose types carry lifetimes, checked by local region inference.
  R2  A Rue function signature may mention borrows only in shapes Rust's
      lifetime-elision rules produce. Rue source never names a lifetime.
  R3  Rue-defined types never have lifetime parameters.

Questions answered here (signature level only):
  Q1  Which std functions could Rue *declare* with the same signature?
  Q2  Which std types carry lifetime parameters (Rue may hold them in locals
      but could not define them)?
  Q3  Which std trait impls could a Rue type write without naming a lifetime?

Run: python3 -I classify.py <json-dir> <out.json>   (then report.py <out.json>)
     python3 -I classify.py <json-dir> --borrowing-types (methods on lifetime-carrying types)
"""

import collections
import json
import sys

STATIC = "'static"
ANON = "'_"

# --------------------------------------------------------------------------
# Type walking
# --------------------------------------------------------------------------


class Slots:
    """Lifetime positions found while walking a type.

    Each entry is (lifetime, ctx) where lifetime is a name such as "'a",
    "'static", "'_" or None (an elided `&T`), and ctx tags the position.
    """

    def __init__(self):
        self.slots = []
        self.flags = set()

    def add(self, lt, ctx):
        self.slots.append((lt, ctx))


def walk_type(t, out, ctx="plain", hrtb=frozenset()):
    """Collect lifetime slots and feature flags from a rustdoc JSON type."""
    if t is None:
        return
    if isinstance(t, str):
        return
    if not isinstance(t, dict):
        return
    (kind, v), = t.items()
    if kind == "borrowed_ref":
        lt = v["lifetime"]
        if lt in hrtb:
            lt = ("hrtb", lt)
        out.add(lt, "mut-ref" if v["is_mutable"] else "ref")
        walk_type(v["type"], out, "under-mut" if v["is_mutable"] else ctx, hrtb)
    elif kind == "resolved_path":
        walk_path_args(v.get("args"), out, ctx, hrtb)
    elif kind == "dyn_trait":
        out.flags.add("dyn")
        lt = v.get("lifetime")
        if lt is not None:
            out.add(("hrtb", lt) if lt in hrtb else lt, "dyn-bound")
        for pt in v["traits"]:
            inner = hrtb | {p["name"] for p in pt.get("generic_params", [])}
            walk_path_args(pt["trait"].get("args"), out, ctx, inner)
    elif kind == "impl_trait":
        out.flags.add("impl-trait")
        for b in v:
            walk_bound(b, out, hrtb)
    elif kind == "function_pointer":
        out.flags.add("fn-ptr")
        inner = hrtb | {p["name"] for p in v.get("generic_params", [])}
        sub = Slots()
        for _, ty in v["sig"]["inputs"]:
            walk_type(ty, sub, ctx, inner)
        walk_type(v["sig"]["output"], sub, ctx, inner)
        # fn-pointer lifetimes are bound by the pointer type itself
        # (elision applies inside `fn(&T) -> &U`), so only free named
        # lifetimes escape.
        for lt, c in sub.slots:
            if isinstance(lt, str) and lt not in (ANON,) and lt is not None:
                out.add(lt, c)
        out.flags |= sub.flags
    elif kind == "qualified_path":
        out.flags.add("assoc-proj")
        walk_type(v["self_type"], out, ctx, hrtb)
        if v.get("trait"):
            walk_path_args(v["trait"].get("args"), out, ctx, hrtb)
        walk_path_args(v.get("args"), out, ctx, hrtb)
    elif kind in ("tuple",):
        for x in v:
            walk_type(x, out, ctx, hrtb)
    elif kind in ("slice",):
        walk_type(v, out, ctx, hrtb)
    elif kind == "array":
        walk_type(v["type"], out, ctx, hrtb)
    elif kind == "raw_pointer":
        out.flags.add("raw-ptr")
        walk_type(v["type"], out, ctx, hrtb)
    elif kind == "pat":
        walk_type(v.get("type"), out, ctx, hrtb)
    # generic, primitive, infer: nothing to collect


def walk_path_args(args, out, ctx, hrtb):
    if not args:
        return
    if "angle_bracketed" in args:
        ab = args["angle_bracketed"]
        for a in ab["args"]:
            if "lifetime" in a:
                lt = a["lifetime"]
                out.add(("hrtb", lt) if lt in hrtb else lt, "path-arg")
            elif "type" in a:
                walk_type(a["type"], out, ctx, hrtb)
        for c in ab.get("constraints", []):
            walk_path_args(c.get("args"), out, ctx, hrtb)
            b = c.get("binding", {})
            if "equality" in b:
                eq = b["equality"]
                if "type" in eq:
                    walk_type(eq["type"], out, ctx, hrtb)
            elif "constraint" in b:
                for bb in b["constraint"]:
                    walk_bound(bb, out, hrtb)
    elif "parenthesized" in args:
        # Fn(&T) -> &U sugar: elision applies inside, so elided lifetimes
        # are bound by the sugar. Only free named lifetimes escape.
        out.flags.add("fn-sugar")
        sub = Slots()
        p = args["parenthesized"]
        for ty in p["inputs"]:
            walk_type(ty, sub, ctx, hrtb)
        walk_type(p.get("output"), sub, ctx, hrtb)
        for lt, c in sub.slots:
            if isinstance(lt, str) and lt != ANON:
                out.add(lt, "fn-sugar")
        out.flags |= sub.flags
    elif "return_type_notation" in args:
        pass


def walk_bound(b, out, hrtb):
    if "trait_bound" in b:
        tb = b["trait_bound"]
        inner = hrtb | {p["name"] for p in tb.get("generic_params", [])}
        if tb.get("generic_params"):
            out.flags.add("hrtb")
        walk_path_args(tb["trait"].get("args"), out, "bound", inner)
    elif "outlives" in b:
        lt = b["outlives"]
        out.add(("hrtb", lt) if lt in hrtb else lt, "outlives")
    elif "use" in b:
        for a in b["use"]:
            if isinstance(a, dict) and "lifetime" in a:
                out.add(a["lifetime"], "use-bound")


def collect_bounds(generics, out):
    """Lifetimes mentioned by generic-param bounds and where-clauses."""
    for p in generics.get("params", []):
        k = p["kind"]
        if "lifetime" in k:
            for o in k["lifetime"].get("outlives", []):
                out.add(o, "outlives")
                out.add(p["name"], "outlives")
        elif "type" in k:
            for b in k["type"].get("bounds", []):
                walk_bound(b, out, frozenset())
    for wp in generics.get("where_predicates", []):
        if "bound_predicate" in wp:
            bp = wp["bound_predicate"]
            inner = frozenset(p["name"] for p in bp.get("generic_params", []))
            if bp.get("generic_params"):
                out.flags.add("hrtb")
            sub = Slots()
            walk_type(bp["type"], sub, "bound", inner)
            for lt, c in sub.slots:
                out.add(lt, "where-type")
            for b in bp["bounds"]:
                walk_bound(b, out, inner)
        elif "lifetime_predicate" in wp:
            lp = wp["lifetime_predicate"]
            out.add(lp["lifetime"], "outlives")
            for o in lp["outlives"]:
                out.add(o, "outlives")
        elif "eq_predicate" in wp:
            walk_type(wp["eq_predicate"]["lhs"], out, "bound")


def lifetime_params(generics):
    return [p["name"] for p in generics.get("params", []) if "lifetime" in p["kind"]]


def type_has_lifetime(t):
    s = Slots()
    walk_type(t, s)
    return any(lt is not None and not isinstance(lt, tuple) for lt, _ in s.slots) or any(
        lt is None for lt, c in s.slots
    )


def type_lifetime_names(t):
    s = Slots()
    walk_type(t, s)
    return {lt for lt, _ in s.slots if isinstance(lt, str)}


# --------------------------------------------------------------------------
# Function classification
# --------------------------------------------------------------------------

def classify_fn(fn, impl_generics=None, self_type=None):
    """Return (category, reasons, flags) for one function signature.

    Categories:
      C0  no lifetime anywhere in the signature
      C1  borrows in inputs only
      C2  borrows in output, and elision would produce exactly this signature
      C4  needs a named lifetime (reason says why)
    'static is tracked separately as a flag ("static"), because Rue would need
    a spelling for it but it is not a lifetime *parameter*.
    """
    sig = fn["sig"]
    gen = fn["generics"]
    params = set(lifetime_params(gen))
    if impl_generics is not None:
        params |= set(lifetime_params(impl_generics))

    ins = Slots()
    receiver_lt = "none"  # "none" | lifetime slot of &self
    for i, (name, ty) in enumerate(sig["inputs"]):
        before = len(ins.slots)
        walk_type(ty, ins)
        if i == 0 and name == "self":
            if "borrowed_ref" in ty:
                receiver_lt = ty["borrowed_ref"]["lifetime"]  # None or name
                if receiver_lt is None:
                    receiver_lt = "elided"
            # self: Self where Self = &'a T comes through impl self type; ignore
    outs = Slots()
    walk_type(sig["output"], outs)
    bnds = Slots()
    collect_bounds(gen, bnds)

    flags = set(ins.flags | outs.flags | bnds.flags)
    reasons = []

    def free(slots):
        return [(lt, c) for lt, c in slots if not isinstance(lt, tuple)]

    in_slots = free(ins.slots)
    out_slots = free(outs.slots)
    bound_slots = free(bnds.slots)

    if any(lt == STATIC for lt, _ in in_slots + out_slots + bound_slots):
        flags.add("static")

    # Bounds that mention a lifetime parameter (not 'static) cannot be elided.
    for lt, c in bound_slots:
        if lt in params:
            reasons.append(f"bound mentions {lt} ({c})")
        elif lt == ANON:
            reasons.append("anonymous lifetime in bound")
    in_named = collections.Counter(lt for lt, _ in in_slots if lt in params)
    out_named = [lt for lt, _ in out_slots if lt in params]
    in_count = len([1 for lt, _ in in_slots if lt != STATIC])

    has_out = any(lt != STATIC for lt, _ in out_slots)

    if not has_out:
        # Inputs only. A named param used twice ties two inputs together;
        # under elision they would get distinct lifetimes.
        for lt, n in in_named.items():
            if n > 1:
                reasons.append(f"{lt} shared by {n} inputs")
        cat = "C1" if in_count else "C0"
    else:
        # Which lifetime would elision assign to output slots?
        if receiver_lt != "none":
            designated = receiver_lt  # "elided" or a name
            rule = "self"
        elif in_count == 1:
            designated = next(lt for lt, _ in in_slots if lt != STATIC)
            designated = "elided" if designated in (None, ANON) else designated
            rule = "single-input"
        else:
            designated = None
            rule = None
        if designated is None:
            reasons.append(
                "output borrows but no &self and %d input lifetimes" % in_count
            )
        else:
            for lt, _ in out_slots:
                if lt == STATIC:
                    continue
                if lt in (None, ANON):
                    continue  # elided output takes the designated lifetime
                if designated == "elided" or lt != designated:
                    reasons.append(f"output uses {lt}, elision ({rule}) picks another")
                    break
        # The designated named lifetime must appear only once in inputs.
        for lt, n in in_named.items():
            if n > 1:
                reasons.append(f"{lt} shared by {n} inputs")
        # A named param that only appears in the output is unconstrained.
        for lt in set(out_named):
            if in_named[lt] == 0:
                reasons.append(f"{lt} appears only in output")
        cat = "C2"
    if reasons:
        cat = "C4"
    return cat, sorted(set(reasons)), sorted(flags)


# --------------------------------------------------------------------------
# Crate traversal
# --------------------------------------------------------------------------

def is_stable(item):
    st = item.get("stability")
    if not st:
        return None  # unknown (often trait-impl items)
    lvl = st["level"]
    return isinstance(lvl, dict) and "stable" in lvl


def hidden(item):
    for a in item.get("attrs", []):
        if isinstance(a, dict) and a.get("other", "").startswith("#[doc(hidden)"):
            return True
    return False


def type_name(t):
    if not isinstance(t, dict):
        return str(t)
    (k, v), = t.items()
    if k == "resolved_path":
        return v["path"].split("::")[-1]
    if k == "primitive":
        return v
    if k == "borrowed_ref":
        return ("&mut " if v["is_mutable"] else "&") + type_name(v["type"])
    if k == "generic":
        return v
    if k == "slice":
        return "[" + type_name(v) + "]"
    if k == "array":
        return "[" + type_name(v["type"]) + "; N]"
    if k == "raw_pointer":
        return ("*mut " if v["is_mutable"] else "*const ") + type_name(v["type"])
    if k == "tuple":
        return "(" + ", ".join(type_name(x) for x in v) + ")"
    if k == "dyn_trait":
        return "dyn " + "+".join(p["trait"]["path"].split("::")[-1] for p in v["traits"])
    return k


def render_type(t):
    """Compact rendering for reports."""
    if t is None:
        return "()"
    if not isinstance(t, dict):
        return str(t)
    (k, v), = t.items()
    if k == "borrowed_ref":
        lt = (v["lifetime"] + " ") if v["lifetime"] else ""
        return "&" + lt + ("mut " if v["is_mutable"] else "") + render_type(v["type"])
    if k == "resolved_path":
        name = v["path"].split("::")[-1]
        a = v.get("args")
        if a and "angle_bracketed" in a:
            parts = []
            for x in a["angle_bracketed"]["args"]:
                if "lifetime" in x:
                    parts.append(x["lifetime"])
                elif "type" in x:
                    parts.append(render_type(x["type"]))
                else:
                    parts.append("_")
            for c in a["angle_bracketed"].get("constraints", []):
                b = c.get("binding", {})
                if "equality" in b and "type" in b["equality"]:
                    parts.append(c["name"] + " = " + render_type(b["equality"]["type"]))
                else:
                    parts.append(c["name"] + ": ..")
            if parts:
                return name + "<" + ", ".join(parts) + ">"
        if a and "parenthesized" in a:
            p = a["parenthesized"]
            o = p.get("output")
            return name + "(" + ", ".join(render_type(x) for x in p["inputs"]) + ")" + (
                " -> " + render_type(o) if o else ""
            )
        return name
    if k == "generic":
        return v
    if k == "primitive":
        return v
    if k == "slice":
        return "[" + render_type(v) + "]"
    if k == "array":
        return "[" + render_type(v["type"]) + "; " + str(v.get("len", "N")) + "]"
    if k == "tuple":
        return "(" + ", ".join(render_type(x) for x in v) + ")"
    if k == "raw_pointer":
        return ("*mut " if v["is_mutable"] else "*const ") + render_type(v["type"])
    if k == "impl_trait":
        bs = []
        for b in v:
            if "trait_bound" in b:
                bs.append(render_type({"resolved_path": b["trait_bound"]["trait"]}))
            elif "outlives" in b:
                bs.append(b["outlives"])
            elif "use" in b:
                bs.append("use<..>")
        return "impl " + " + ".join(bs)
    if k == "dyn_trait":
        bs = [render_type({"resolved_path": p["trait"]}) for p in v["traits"]]
        if v.get("lifetime"):
            bs.append(v["lifetime"])
        return "dyn " + " + ".join(bs)
    if k == "qualified_path":
        return "<" + render_type(v["self_type"]) + ">::" + v["name"]
    if k == "function_pointer":
        return "fn(..)"
    return k


def render_sig(name, fn):
    gen = fn["generics"]
    gp = [p["name"] for p in gen.get("params", []) if not (
        "type" in p["kind"] and p["kind"]["type"].get("is_synthetic"))]
    ins = []
    for n, t in fn["sig"]["inputs"]:
        if n == "self":
            if t == {"generic": "Self"}:
                ins.append("self")
            elif "borrowed_ref" in t and t["borrowed_ref"]["type"] == {"generic": "Self"}:
                br = t["borrowed_ref"]
                lt = (br["lifetime"] + " ") if br["lifetime"] else ""
                ins.append("&" + lt + ("mut " if br["is_mutable"] else "") + "self")
            else:
                ins.append("self: " + render_type(t))
        else:
            ins.append(n + ": " + render_type(t))
    out = fn["sig"]["output"]
    s = "fn " + name + ("<" + ", ".join(gp) + ">" if gp else "") + "(" + ", ".join(ins) + ")"
    if out is not None:
        s += " -> " + render_type(out)
    return s


def excluded_span(it):
    """core::arch (stdarch), intrinsics and portable SIMD are left out."""
    fn = (it.get("span") or {}).get("filename", "")
    return "/stdarch/" in fn or "/core/src/intrinsics/" in fn or "/portable-simd/" in fn


def survey(json_dir):
    crates = ["core", "alloc", "std"]
    results = {
        "functions": [],
        "types": [],
        "impls": [],
        "traits": [],
    }
    for cname in crates:
        d = json.load(open(f"{json_dir}/{cname}.json"))
        idx = d["index"]
        paths = d["paths"]
        # parent maps
        parent = {}
        for iid, it in idx.items():
            inner = it["inner"]
            if "impl" in inner:
                for m in inner["impl"]["items"]:
                    parent[str(m)] = ("impl", iid)
            elif "trait" in inner:
                for m in inner["trait"]["items"]:
                    parent[str(m)] = ("trait", iid)

        def item_path(iid):
            p = paths.get(str(iid))
            return "::".join(p["path"]) if p else None

        for iid, it in idx.items():
            if it["crate_id"] != 0:
                continue
            inner = it["inner"]
            if excluded_span(it) or hidden(it):
                continue
            kind = next(iter(inner))
            if kind in ("struct", "enum", "union") and it["visibility"] == "public":
                if is_stable(it) is not True:
                    continue
                gen = inner[kind]["generics"]
                lts = lifetime_params(gen)
                results["types"].append({
                    "crate": cname, "path": item_path(iid) or it["name"],
                    "kind": kind, "lifetimes": lts,
                    "deprecated": it.get("deprecation") is not None,
                })
            elif kind == "trait" and it["visibility"] == "public":
                if is_stable(it) is not True:
                    continue
                tr = inner["trait"]
                methods = []
                for m in tr["items"]:
                    mi = idx.get(str(m))
                    if not mi or "function" not in mi["inner"]:
                        continue
                    cat, why, fl = classify_fn(mi["inner"]["function"], tr["generics"], None)
                    methods.append({"name": mi["name"], "cat": cat, "why": why,
                                    "required": not mi["inner"]["function"]["has_body"],
                                    "sig": render_sig(mi["name"], mi["inner"]["function"])})
                results["traits"].append({
                    "crate": cname, "path": item_path(iid) or it["name"],
                    "lifetimes": lifetime_params(tr["generics"]),
                    "is_auto": tr.get("is_auto", False),
                    "methods": methods,
                })
            elif kind == "impl":
                im = inner["impl"]
                if im["is_synthetic"] or im["blanket_impl"] is not None:
                    continue
                if im["trait"] is None:
                    continue  # inherent impls are handled via their methods
                if im.get("is_negative"):
                    continue
                st = is_stable(it)
                if st is False:
                    continue
                self_ty = im["for"]
                impl_lts = lifetime_params(im["generics"])
                self_lts = type_lifetime_names(self_ty)
                self_is_ref = "borrowed_ref" in self_ty
                trait_args = Slots()
                walk_path_args(im["trait"].get("args"), trait_args, "plain", frozenset())
                # where do impl lifetime params get used?
                uses = collections.Counter()
                assoc_uses = set()
                method_uses = set()
                for m in im["items"]:
                    mi = idx.get(str(m))
                    if not mi:
                        continue
                    mk = next(iter(mi["inner"]))
                    if mk == "assoc_type":
                        at = mi["inner"]["assoc_type"]
                        if at.get("type") is not None:
                            for lt in type_lifetime_names(at["type"]):
                                if lt in impl_lts:
                                    assoc_uses.add(lt)
                    elif mk == "function":
                        f = mi["inner"]["function"]
                        s = Slots()
                        for _, t in f["sig"]["inputs"]:
                            walk_type(t, s)
                        walk_type(f["sig"]["output"], s)
                        collect_bounds(f["generics"], s)
                        for lt, _ in s.slots:
                            if lt in impl_lts:
                                method_uses.add(lt)
                bound_s = Slots()
                collect_bounds(im["generics"], bound_s)
                bound_uses = {lt for lt, _ in bound_s.slots if lt in impl_lts}
                header_counts = collections.Counter(
                    [lt for lt in type_lifetime_names_list(self_ty) if lt in impl_lts] +
                    [lt for lt, _ in trait_args.slots if lt in impl_lts])
                trait_name = im["trait"]["path"].split("::")[-1]
                if self_lts - {STATIC} and not self_is_ref:
                    cat = "I2-self-type-has-lifetime"
                elif self_is_ref and (type_lifetime_names(self_ty["borrowed_ref"]["type"]) - {STATIC}):
                    cat = "I2-self-type-has-lifetime"
                elif not impl_lts:
                    cat = "I0-no-lifetimes"
                else:
                    needs_name = set()
                    for lt in impl_lts:
                        if lt in assoc_uses or lt in bound_uses or header_counts[lt] > 1:
                            needs_name.add(lt)
                        elif lt in method_uses:
                            needs_name.add(lt)
                    if not needs_name:
                        cat = "I1-header-elision-ok"
                    elif self_is_ref:
                        cat = "I3-impl-for-&Self-needs-name"
                    else:
                        cat = "I3-trait-args-need-name"
                results["impls"].append({
                    "crate": cname,
                    "trait": trait_name,
                    "for": render_type(self_ty),
                    "cat": cat,
                })
            elif kind == "function" and it["visibility"] in ("public", "default"):
                par = parent.get(iid)
                if par and par[0] == "trait":
                    continue  # counted with the trait
                impl_gen = None
                self_ty = None
                owner = None
                if par and par[0] == "impl":
                    pim = idx[par[1]]["inner"]["impl"]
                    if pim["trait"] is not None:
                        continue  # trait impl methods: signature set by the trait
                    if pim["is_synthetic"] or pim["blanket_impl"] is not None:
                        continue
                    impl_gen = pim["generics"]
                    self_ty = pim["for"]
                    owner = render_type(self_ty)
                    pit = idx[par[1]]
                    if excluded_span(pit):
                        continue
                elif it["visibility"] != "public":
                    continue
                if is_stable(it) is not True:
                    continue
                f = inner["function"]
                cat, why, fl = classify_fn(f, impl_gen, self_ty)
                self_lt = bool(self_ty is not None and (type_lifetime_names(self_ty) - {STATIC}))
                self_prim = bool(self_ty is not None and (
                    "primitive" in self_ty or
                    ("slice" in self_ty) or ("array" in self_ty)))
                results["functions"].append({
                    "crate": cname,
                    "owner": owner,
                    "name": it["name"],
                    "path": item_path(iid),
                    "cat": cat,
                    "why": why,
                    "flags": fl,
                    "self_has_lifetime": self_lt,
                    "self_primitive": self_prim,
                    "deprecated": it.get("deprecation") is not None,
                    "unsafe": f["header"]["is_unsafe"],
                    "const": f["header"]["is_const"],
                    "sig": render_sig(it["name"], f),
                    "type_params": len([p for p in f["generics"]["params"] if "type" in p["kind"]]),
                })
    return results


def type_lifetime_names_list(t):
    s = Slots()
    walk_type(t, s)
    return [lt for lt, _ in s.slots if isinstance(lt, str)]





# --------------------------------------------------------------------------
# Borrowing-type analysis (refined rule): a type with exactly one lifetime
# parameter is a "borrowing type"; how many of its inherent methods are
# expressible with
#   V1  plain elision from &self / &mut self
#   V2  by-value self whose outputs inherit the type's lifetime ("copy(self)")
#   V3  &self receiver but output uses the type's lifetime (needs "copy(self)")
#   V4  anything else
# --------------------------------------------------------------------------

def classify_borrowing_type_method(fn, impl_generics):
    impl_lts = lifetime_params(impl_generics)
    fn_lts = lifetime_params(fn["generics"])
    if len(impl_lts) != 1:
        return "V4-multi-lifetime-type"
    v = impl_lts[0]
    sig = fn["sig"]
    recv = None
    if sig["inputs"] and sig["inputs"][0][0] == "self":
        t = sig["inputs"][0][1]
        if "borrowed_ref" in t:
            recv = "ref"
        else:
            recv = "value"
    outs = Slots()
    walk_type(sig["output"], outs)
    ins = Slots()
    for i, (_, t) in enumerate(sig["inputs"]):
        if i == 0 and recv is not None:
            continue
        walk_type(t, ins)
    b = Slots()
    collect_bounds(fn["generics"], b)
    named_out = {lt for lt, _ in outs.slots if isinstance(lt, str) and lt not in (ANON, STATIC)}
    elided_out = [lt for lt, _ in outs.slots if lt in (None, ANON)]
    if fn_lts or any(isinstance(lt, str) and lt in fn_lts for lt, _ in b.slots):
        return "V4-method-lifetime-params"
    if any(lt == v for lt, _ in b.slots):
        return "V4-type-lifetime-in-bound"
    if named_out - {v}:
        return "V4-other"
    other_in = [lt for lt, _ in ins.slots if lt is not None and lt != STATIC and lt != ANON and lt != v]
    if v in named_out:
        if recv == "value":
            return "V2-by-value-self-inherits"
        if recv == "ref":
            return "V3-ref-self-returns-type-lifetime"
        return "V4-no-receiver"
    if elided_out:
        if recv == "ref":
            return "V1-plain-elision"
        return "V4-elided-output-without-ref-receiver"
    return "V1-plain-elision"


def survey_borrowing_types(json_dir):
    out = collections.Counter()
    single = collections.Counter()
    examples = collections.defaultdict(list)
    for cname in ["core", "alloc", "std"]:
        d = json.load(open(f"{json_dir}/{cname}.json"))
        idx = d["index"]
        for iid, it in idx.items():
            if it["crate_id"] != 0:
                continue
            inner = it["inner"]
            if excluded_span(it) or hidden(it):
                continue
            kind = next(iter(inner))
            if kind in ("struct", "enum", "union") and it["visibility"] == "public":
                if is_stable(it) is not True or it.get("deprecation"):
                    continue
                n = len(lifetime_params(inner[kind]["generics"]))
                if n:
                    single[n] += 1
            if kind != "impl":
                continue
            im = inner["impl"]
            if im["trait"] is not None or im["is_synthetic"] or im["blanket_impl"] is not None:
                continue
            if not (type_lifetime_names(im["for"]) - {STATIC}):
                continue
            for m in im["items"]:
                mi = idx.get(str(m))
                if not mi or "function" not in mi["inner"]:
                    continue
                if is_stable(mi) is not True or mi.get("deprecation"):
                    continue
                c = classify_borrowing_type_method(mi["inner"]["function"], im["generics"])
                out[c] += 1
                if len(examples[c]) < 6:
                    examples[c].append(render_type(im["for"]) + " :: " + render_sig(mi["name"], mi["inner"]["function"]))
    return out, single, examples


if __name__ == "__main__":
    if sys.argv[2] == "--borrowing-types":
        out, single, ex = survey_borrowing_types(sys.argv[1])
        print("stable non-deprecated types by lifetime-parameter count:", dict(sorted(single.items())))
        tot = sum(out.values())
        print("inherent methods on lifetime-carrying types:", tot)
        for k, v in sorted(out.items()):
            print(f"  {k}: {v} ({100*v/tot:.0f}%)")
            for e in ex[k]:
                print("      ", e)
    else:
        res = survey(sys.argv[1])
        json.dump(res, open(sys.argv[2], "w"), indent=1)
        print("functions", len(res["functions"]), "types", len(res["types"]),
              "impls", len(res["impls"]), "traits", len(res["traits"]))
