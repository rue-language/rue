#!/usr/bin/env python3
"""Self-tests for validate-payload-ownership.py."""

from __future__ import annotations

import importlib.util
import tempfile
from pathlib import Path


SCRIPT = Path(__file__).with_name("validate-payload-ownership.py")
SPEC = importlib.util.spec_from_file_location("payload_inventory", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def roots(base: Path) -> list[tuple[str, Path]]:
    result = []
    for crate in MODULE.OWNER_PATHS:
        root = base / crate
        root.mkdir()
        result.append((crate, root))
    return result


def errors_for(source: str, crate: str = "rue-codegen", relative: str = "consumer.rs") -> list[str]:
    with tempfile.TemporaryDirectory() as temporary:
        base = Path(temporary)
        sources = roots(base)
        path = base / crate / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source)
        return MODULE.validate(sources)


def main() -> None:
    assert not errors_for("pub fn typed() -> usize { 0 }\n")
    cases = {
        "direct side-table access": "fn bad(cfg: &Cfg) { cfg.values.push(x); }\n",
        "raw range construction": "fn bad() { OtherRange::from_parts(0, 1); }\n",
        "raw enum cast": "fn bad() { let _ = PatternKind::Path as u32; }\n",
        "raw public accessor": "pub fn words() -> &[u32] { todo!() }\n",
        "allocating getter": "pub fn get_payload() -> Vec<u32> { vec![] }\n",
        "lifetime raw CFG": "pub fn lower(cfg: &'a Cfg) {}\n",
        "qualified raw CFG": "pub fn lower(cfg: &rue_cfg::Cfg) {}\n",
        "mutable non-u32 store": "pub fn values_mut() -> &mut Vec<CfgValue> { todo!() }\n",
    }
    for label, source in cases.items():
        assert errors_for(source), f"validator missed {label}"
    assert errors_for(
        "pub struct Air {\n    pub places: Vec<AirPlace>,\n}\n",
        "rue-air",
        "inst.rs",
    ), "validator missed AIR place store field"
    assert not errors_for(
        "fn rollback(rir: &mut Rir) { rir.extra.truncate(0); }\n",
        "rue-rir",
        "inst/packed.rs",
    ), "validator rejected the reviewed packed-RIR owner extension"
    assert errors_for(
        "pub struct PackedOwner {\n    pub extra: Vec<u32>,\n}\n",
        "rue-rir",
        "inst/packed.rs",
    ), "validator allowed a public store from the packed-RIR owner extension"
    print("payload ownership inventory tool tests passed")


if __name__ == "__main__":
    main()
