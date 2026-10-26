"""Aggregate classify.py output into the numbers used by the research note."""

import collections
import json
import re
import sys

r = json.load(open(sys.argv[1]))
NUM = r"\b(u8|u16|u32|u64|u128|usize|i8|i16|i32|i64|i128|isize|f16|f32|f64|f128)\b"


def norm(s):
    s = re.sub(r"Atomic(Bool|Ptr|I8|I16|I32|I64|I128|Isize|U8|U16|U32|U64|U128|Usize)\b", "Atomic{x}", s or "")
    s = re.sub(r"NonZero(I|U)(8|16|32|64|128|size)\b", "NonZero{x}", s)
    s = re.sub(r"\b(bool)\b", "{num}", s)
    return re.sub(NUM, "{num}", s)


def bucket(f):
    if f["self_has_lifetime"]:
        return "C3"
    return f["cat"]


def dedupe(fs):
    seen = {}
    for f in fs:
        owner = norm(re.sub(r"<.*", "", f["owner"] or "") if f["owner"] else (f["path"] or ""))
        key = (owner, f["name"], norm(f["sig"]))
        seen.setdefault(key, f)
    return list(seen.values())


funcs = [f for f in r["functions"] if not f["deprecated"]]
uniq = dedupe(funcs)

print("== Functions (stable, non-deprecated; inherent methods + free fns) ==")
for label, fs in (("raw", funcs), ("distinct", uniq)):
    c = collections.Counter(bucket(f) for f in fs)
    n = len(fs)
    print(f"{label}: n={n}", {k: f"{v} ({100*v/n:.1f}%)" for k, v in sorted(c.items())})
    static = sum(1 for f in fs if bucket(f) in ("C0", "C1", "C2") and "static" in f["flags"])
    print(f"  of C0-C2, using 'static: {static}")
    by_crate = collections.defaultdict(collections.Counter)
    for f in fs:
        by_crate[f["crate"]][bucket(f)] += 1
    for k, v in by_crate.items():
        tot = sum(v.values())
        print("  ", k, tot, {kk: f"{vv} ({100*vv/tot:.0f}%)" for kk, vv in sorted(v.items())})

print()
print("== Feature flags among distinct C0-C2 (what else Rue needs to declare them) ==")
ok = [f for f in uniq if bucket(f) in ("C0", "C1", "C2")]
fl = collections.Counter(x for f in ok for x in f["flags"])
print(len(ok), dict(fl.most_common()))
print("  with type params:", sum(1 for f in ok if f["type_params"] > 0))
print("  unsafe:", sum(1 for f in ok if f["unsafe"]))

print()
print("== C4 (needs a named lifetime; Self type lifetime-free) ==")
c4 = [f for f in uniq if bucket(f) == "C4"]
reasons = collections.Counter()
for f in c4:
    keyset = set()
    for w in f["why"]:
        if "only in output" in w:
            keyset.add("unbounded output lifetime")
        elif w.startswith("bound mentions") or "anonymous lifetime in bound" in w:
            keyset.add("lifetime in a bound/where-clause")
        elif "no &self" in w:
            keyset.add("output tied to one of several inputs")
        elif "shared by" in w:
            keyset.add("one lifetime shared by several inputs")
        elif "elision" in w:
            keyset.add("output tied to a non-self input")
        else:
            keyset.add(w)
    for k in keyset:
        reasons[k] += 1
print(len(c4), dict(reasons.most_common()))
unsafe_c4 = sum(1 for f in c4 if f["unsafe"])
print("  unsafe among C4:", unsafe_c4)
for f in sorted(c4, key=lambda f: (f["owner"] or f["path"] or "")):
    print(f"  {'U' if f['unsafe'] else ' '} {(f['owner'] or f['path'])}: {f['sig']}  -- {'; '.join(f['why'])}")

print()
print("== C3 owners (types with lifetime params that own inherent methods) ==")
c3 = collections.Counter(re.sub(r"<.*", "", f["owner"]) for f in uniq if bucket(f) == "C3")
print(len(c3), c3.most_common(60))

print()
print("== Types ==")
types = [t for t in r["types"] if not t["deprecated"]]
with_lt = [t for t in types if t["lifetimes"]]
print(f"public stable structs/enums/unions: {len(types)}; with lifetime params: {len(with_lt)} ({100*len(with_lt)/len(types):.1f}%)")


def family(name):
    n = name.split("::")[-1]
    if re.search(r"(Iter|IterMut|IntoIter|Keys|Values|ValuesMut|Range|RangeMut|Drain|Chars|CharIndices|Bytes|Lines|Split\w*|RSplit\w*|Matches|RMatches|MatchIndices|RMatchIndices|Chunks\w*|RChunks\w*|Windows|ArrayWindows|EscapeDebug|EscapeDefault|EscapeUnicode|EncodeUtf16|Components|Ancestors|Incoming|Args|Difference|Intersection|Union|SymmetricDifference|ExtractIf|Splice|ReadDir|CommandArgs|CommandEnvs|Utf8Chunks|SplitInclusive|SplitWhitespace|SplitAsciiWhitespace|SplitTerminator|SplitN|RSplitN|EscapeAscii|GroupBy|ChunkBy|Peekable|Cursor|CursorMut)$", n):
        return "iterator/cursor"
    if re.search(r"(Guard|Lock|Ref|RefMut|Mapped\w*|StdinLock|StdoutLock|StderrLock|ReentrantLockGuard)$", n):
        return "guard/lock"
    if re.search(r"(Entry|OccupiedEntry|VacantEntry|OccupiedError|PeekMut)$", n):
        return "entry/peek"
    if n in ("Formatter", "Arguments", "DebugStruct", "DebugTuple", "DebugList", "DebugSet", "DebugMap", "Display", "FormattingOptions"):
        return "fmt"
    if n in ("Cow",):
        return "cow"
    if re.search(r"(Scope|ScopedJoinHandle)$", n):
        return "scoped threads"
    if n in ("IoSlice", "IoSliceMut", "BorrowedFd", "BorrowedHandle", "BorrowedSocket", "BorrowedBuf", "BorrowedCursor", "PanicHookInfo", "PanicInfo", "Location", "Context", "Request", "Source", "PhantomContravariantLifetime", "PhantomCovariantLifetime", "PhantomInvariantLifetime", "VaList", "SocketAddr"):
        return "borrowed handle/context"
    return "other"


fam = collections.Counter(family(t["path"]) for t in with_lt)
print(dict(fam.most_common()))
for t in sorted(with_lt, key=lambda t: (family(t["path"]), t["path"])):
    if family(t["path"]) in ("other", "borrowed handle/context", "fmt", "cow", "scoped threads", "entry/peek", "guard/lock"):
        print("   ", family(t["path"]), t["path"], t["lifetimes"])

print()
print("== Traits: implementable by a lifetime-free Rue type without naming a lifetime? ==")
traits = r["traits"]
yes, no = [], []
for t in traits:
    if t["is_auto"]:
        continue
    bad = [m for m in t["methods"] if m["required"] and m["cat"] == "C4"]
    if t["lifetimes"]:
        no.append((t, "trait has lifetime params " + ",".join(t["lifetimes"])))
    elif bad:
        no.append((t, "; ".join(m["sig"] for m in bad)))
    else:
        yes.append(t)
print(f"stable non-auto traits: {len(yes)+len(no)}; implementable: {len(yes)}; not: {len(no)}")
for t, why in no:
    print("   ", t["path"], "--", why)

print()
print("== Trait impls in std by category ==")
imps = r["impls"]
c = collections.Counter(i["cat"] for i in imps)
print(len(imps), dict(c.most_common()))
for cat in ("I3-impl-for-&Self-needs-name", "I3-trait-args-need-name", "I1-header-elision-ok"):
    tc = collections.Counter(i["trait"] for i in imps if i["cat"] == cat)
    print(" ", cat, tc.most_common(25))
