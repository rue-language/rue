#!/usr/bin/env python3
"""Validate the ADR registry and optionally regenerate its index."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
ADR_DIR = ROOT / "docs" / "designs"
README = ADR_DIR / "README.md"
START = "<!-- ADR-INDEX:START -->"
END = "<!-- ADR-INDEX:END -->"
ALLOWED_STATUSES = {"proposal", "accepted", "implemented", "stable", "superseded", "rejected"}
RELATION_KEYS = {"supersedes", "superseded-by", "amends", "amended-by"}


def unquote(value: str) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        return value[1:-1]
    return value


def parse_list(value: str) -> list[str]:
    value = value.strip()
    if not value:
        return []
    if value.startswith("[") and value.endswith("]"):
        value = value[1:-1]
        return [unquote(item.strip()) for item in value.split(",") if item.strip()]
    return [unquote(value)]


def parse_frontmatter(path: Path) -> dict[str, str]:
    lines = path.read_text().splitlines()
    if not lines or lines[0] != "---":
        raise ValueError("missing YAML frontmatter")
    try:
        end = lines.index("---", 1)
    except ValueError as error:
        raise ValueError("unterminated YAML frontmatter") from error

    fields: dict[str, str] = {}
    for line in lines[1:end]:
        if not line or line.lstrip().startswith("#"):
            continue
        match = re.match(r"^([a-z][a-z0-9-]*):\s*(.*?)\s*$", line)
        if not match:
            raise ValueError(f"unsupported frontmatter line: {line!r}")
        key, value = match.groups()
        fields[key] = re.sub(r"\s+#.*$", "", value).strip()
    return fields


def build_index(adrs: list[tuple[Path, dict[str, str]]]) -> str:
    rows = [
        START,
        "| ID | Title | Status | Tags |",
        "| --- | --- | --- | --- |",
    ]
    for path, fields in adrs:
        tags = ", ".join(parse_list(fields.get("tags", ""))) or "—"
        status = fields["status"].capitalize()
        rows.append(f"| [{fields['id']}]({path.name}) | {unquote(fields['title'])} | {status} | {tags} |")
    rows.append(END)
    return "\n".join(rows)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true", help="regenerate the README index")
    args = parser.parse_args()

    errors: list[str] = []
    adrs: list[tuple[Path, dict[str, str]]] = []
    ids: dict[str, Path] = {}
    paths = sorted(ADR_DIR.glob("[0-9][0-9][0-9][0-9]-*.md"))

    for path in paths:
        try:
            fields = parse_frontmatter(path)
        except ValueError as error:
            errors.append(f"{path.relative_to(ROOT)}: {error}")
            continue
        for key in ("id", "title", "status", "tags", "created"):
            if not fields.get(key):
                errors.append(f"{path.relative_to(ROOT)}: missing required field {key!r}")
        adr_id = unquote(fields.get("id", ""))
        status = unquote(fields.get("status", ""))
        fields["id"] = adr_id
        fields["status"] = status
        if status not in ALLOWED_STATUSES:
            errors.append(f"{path.relative_to(ROOT)}: invalid status {status!r}")
        if adr_id in ids:
            errors.append(f"duplicate ADR id {adr_id!r}: {ids[adr_id].name}, {path.name}")
        elif adr_id:
            ids[adr_id] = path
        adrs.append((path, fields))

    for path, fields in adrs:
        for key in RELATION_KEYS:
            for target in parse_list(fields.get(key, "")):
                normalized = target.removeprefix("ADR-")
                normalized = normalized.removesuffix(".md")
                if normalized in ids:
                    continue
                by_filename = any(candidate.stem == normalized for candidate, _ in adrs)
                if not by_filename:
                    errors.append(
                        f"{path.relative_to(ROOT)}: {key} target {target!r} does not resolve"
                    )

    expected = build_index(adrs)
    readme = README.read_text()
    if START not in readme or END not in readme:
        errors.append("docs/designs/README.md: missing generated index markers")
    else:
        before, remainder = readme.split(START, 1)
        _, after = remainder.split(END, 1)
        actual = START + remainder.split(END, 1)[0] + END
        if actual != expected:
            if args.write:
                README.write_text(before + expected + after)
            else:
                errors.append("docs/designs/README.md: ADR index is stale; run scripts/validate-adrs.py --write")

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"ADR registry valid: {len(adrs)} records")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
