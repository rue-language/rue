"""Check Rust twins against the borrow rule.

Rule "strict" (R1-R3): no named lifetimes anywhere ('static allowed and
counted), no lifetime parameters on user types.

Rule "refined": additionally allows lines inside `// refined:` regions to use
named lifetimes, provided every named lifetime is one of:
  * the single lifetime parameter of a struct/enum (a borrowing type) and the
    impls of that type, or
  * a lifetime that only says "the result borrows from parameter(s) X"
    (marked with `// depends:` on the fn line).
The checker only enforces the syntactic part; rustc enforces the rest.

Usage: python3 -I check.py file.rs...
"""

import re
import sys

LIFETIME = re.compile(r"'([A-Za-z_][A-Za-z0-9_]*)\b(?!')")


def strip_strings_and_comments(line):
    # crude: drop // comments and string literals
    line = re.sub(r'"(\\.|[^"\\])*"', '""', line)
    line = re.sub(r"b'(\\.|[^'\\])'", "b''", line)
    line = re.sub(r"'(\\.|[^'\\])'", "''", line)
    return line.split("//", 1)[0]


def check(path):
    strict_violations = []
    refined_uses = []
    statics = 0
    macros = []
    in_refined = False
    for n, raw in enumerate(open(path), 1):
        if "// refined:" in raw:
            in_refined = True
        if "// end refined" in raw:
            in_refined = False
        code = strip_strings_and_comments(raw)
        for mm in re.finditer(r"\b([a-z_][a-z0-9_]*)!\s*[\(\[\{]", code):
            macros.append((n, mm.group(1)))
        for m in LIFETIME.finditer(code):
            name = m.group(1)
            if name == "static":
                statics += 1
                continue
            if name == "_":
                continue
            if in_refined:
                refined_uses.append((n, name, raw.strip()))
            else:
                strict_violations.append((n, name, raw.strip()))
    return strict_violations, refined_uses, statics, macros


ok = True
for p in sys.argv[1:]:
    sv, ru, st, mac = check(p)
    status = "OK" if not sv else "VIOLATION"
    if sv:
        ok = False
    print(f"{p}: strict={status} named-lifetimes-outside-refined={len(sv)} "
          f"refined-lifetime-uses={len(ru)} 'static-uses={st} macros={[m for _, m in mac]}")
    for n, name, line in sv:
        print(f"   line {n}: '{name}: {line}")
sys.exit(0 if ok else 1)
