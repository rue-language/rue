# Explicit module manifests

Rue has an opt-in explicit module manifest mode for build systems that already
know a complete import closure. It is selected with
`--module-manifest <path>`. The existing `@import` spelling and ordinary
filesystem discovery remain the default. `--source-manifest` continues to be a
read allowlist and is not reinterpreted as an import graph.

The current daemon transport does not carry explicit manifest inputs. The CLI
therefore runs manifest requests directly in `--daemon=auto` mode and rejects
`--daemon=required` with an explicit error; integrations can migrate the
transport after the direct mode has demonstrated fresh/warm equivalence.
The current benchmark provenance envelope likewise has no manifest-binding
input field, so `--module-manifest` cannot be combined with
`--benchmark-json`.
The CLI watch loop also remains on the filesystem observer path; explicit
manifests are available through the retained host API for controlled warm
reobservation, while `--watch --module-manifest` is rejected.

Version 1 is deterministic JSON, limited to 16 MiB when read, with a version
field and four data fields:

* `root` is the logical root module identity.
* `modules` maps each logical module identity to a relative declared source
  path. Paths are resolved relative to the captured project root (the
  positional root's directory) and are read again for every request.
* `imports` maps an importer and the exact decoded string value of each
  `@import` literal to a target identity. Importer and target references use
  typed escaped strings: `project:<escaped-logical-id>` for project modules
  and `std:<escaped-relative-path>` for captured trusted modules. `null` means
  that the build system intentionally recorded a missing import. Module
  declarations retain logical identities verbatim, so a legal project module
  named `std:helper.rue` remains representable as
  `project:std%3Ahelper.rue`.
* `std_requirements` lists relative files that must be acquired from the
  independently supplied `--manifest-std-root`.

For a root `main.rue` containing `@import("helper.rue")`, the generated format
is:

```json
{
  "version": 1,
  "root": "main.rue",
  "modules": [
    { "module": "helper.rue", "path": "helper.rue" },
    { "module": "main.rue", "path": "main.rue" }
  ],
  "imports": [
    {
      "importer": "project:main.rue",
      "literal": "helper.rue",
      "target": "project:helper.rue"
    }
  ],
  "std_requirements": []
}
```

Version 1 preserves the existing source identities. Generate them with the
command below; the prototype checks each identity against the current host's
source-admission rules. Repeated occurrences of the same import literal in
one module share one binding.

The compiler does not trust serialized bytes, hashes, offsets, canonical paths,
visibility, provenance, or authority. It reads each declared path, checks its
stable identity and content under the current host policy, parses the complete
set through the canonical session, and requires the serialized bindings to
cover every parsed import exactly. A missing binding, changed import literal,
duplicate key, conflicting identity, missing source, directory, traversal, or
unused module is a stale or incomplete manifest error. Cycles are legal. The
root's transitive closure is the only semantic root; extra entries never add
roots.

Standard-library entries are captured separately. A serialized `std:<path>`
reference is trusted only when the corresponding relative requirement is read
from the caller-supplied `--manifest-std-root`. `RUE_STD_PATH` is ignored in this
mode, and no serialized field can retarget the supplied root or grant trust.
The current std acquisition policy remains responsible for toolchain support
requested later by reached semantic bodies; an incomplete captured set fails
the request without falling back to candidate probing.

Build rules can generate a manifest with `--emit module-manifest` after the
normal discovery request and any reached-body support acquisition closes.
Generation uses the same parser-owned import occurrences and canonical graph
used by compilation. It is an explicit output operation; ordinary compilation
never writes a manifest. To generate and consume one for a project without
toolchain requirements:

```bash
RUE_COMPILER="$(scripts/rue-bin)"
"$RUE_COMPILER" --emit module-manifest main.rue > modules.json
"$RUE_COMPILER" --module-manifest modules.json main.rue -o program
```

When the generated manifest has standard-library requirements, supply their
trusted root with `--manifest-std-root` when consuming it. A migration can keep
the old mode as a compatibility path, run both modes on fresh and warm builds,
and compare compiler, object, and executable bytes before making manifest mode
the default. Module-body edits reuse the manifest; import-key or topology edits
require regeneration.

The explicit path performs one complete source acquisition followed by the
existing adaptive parallel parse fanout. It does not add a parser pool or
publish one revision per import frontier. Measure the existing baseline and
the prototype with a release compiler:

```bash
RUE_COMPILER="$(scripts/rue-bin --target-platforms //platforms:release)"
python3 scripts/bench-module-manifest.py "$RUE_COMPILER" --output /tmp/rue-manifest-measurements
```

The output directory must be empty. By default the script measures forward
and reverse deep chains and wide closures of 33, 129, and 513 modules, using
one and four workers. It interleaves five filesystem and manifest samples per
configuration and measures generation separately. Each invocation creates a
fresh compiler session; operating-system file caches are warm. These timings
do not measure retained-session reuse, which has separate host tests. The
output records compiler and input hashes, raw timing reports, individual
samples, and medians. It checks executable bytes and native behavior across
both modes and a permutation of manifest entries. Inclusive `--time-passes`
rows can overlap and must not be summed. These are external prototype
measurements, separate from the established benchmark JSON contract.

The [2026-09-11 measurements](../notes/module-manifest-measurements.md) record
the initial release comparison and its host and workload limits.

Dependency output preserves semantic topology and source dependencies; operational
filesystem observation history may differ between the two origins by design.
Repeated fresh, warm, body-edit, import-edit, relocation, and metadata-order
runs should compare semantic graph and compiler/object/executable bytes before
switching a build integration to the opt-in mode. Unused manifest entries are
rejected, while captured standard-library support entries remain non-root
inputs and are admitted only through the supplied trusted root.
