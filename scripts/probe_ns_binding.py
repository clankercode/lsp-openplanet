#!/usr/bin/env python3
"""B096 claim 1 — does the engine bind an export stub to a NAMESPACED body?

    scripts/probe_ns_binding.py [--json out.json] [--tsv out.tsv]

B087 taught the unused analysis to root a namespaced body from a namespaced
stub BY SIMPLE NAME, and nobody had asked the game whether that binding is
real. Four shipped plugins are written as though it is —
tm-editor-plus-plus wraps `namespace Editor { ... }` around both the stubs
and the implementations throughout — so one of two things is true:

  * the engine binds them, and B087 is correct; or
  * it does not, and those plugins have dead exports.

Either answer is worth having, and the second is a finding about shipped
code rather than about this repo.

Every one of B086's 18 measured variants uses a PLAIN GLOBAL body, so the
namespaced body is precisely the dimension never exercised. This script
reuses B086's harness — its oracle, its per-variant consumer plugins, its
B077 staging — and swaps only the variant set, because that harness's two
controls are what make any row evidence at all.

Distinct MODULE / exporter id / token from B086 deliberately: a stranded
B086 fixture declaring the same names must not be able to answer a question
asked here (B077's contamination rule).
"""

from __future__ import annotations

import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import probe_export_binding as peb  # noqa: E402
import issue_repro_game as repro  # noqa: E402

# Own identity, so nothing installed from a B086 run can satisfy these.
peb.MODULE = "B096NsProbe"
peb.EXPORTER_ID = repro.PROBE_PREFIX + "b096-exporter"
peb.TOKEN = "B096PROBE"

INNER = "Editor"  # the namespace tm-editor-plus-plus actually uses

# Which variants want their STUB nested one namespace deeper, mirroring
# `namespace Editor { import ... }` in a shipped export file.
NESTED_STUB = {"ns-both-eplusplus-shape"}


def write_exporter(root: Path, variants: list) -> Path:
    """B086's exporter, plus per-variant nesting of the stub.

    B086's version puts every stub in one flat `namespace MODULE { }`. The
    shipped shape this ticket is about nests a second namespace inside it,
    so the stub is `MODULE::Editor::Fn` and the body is `Editor::Fn`.
    """
    plugin = root / "exporter"
    (plugin / "src").mkdir(parents=True)
    (plugin / "info.toml").write_text(
        "[meta]\n"
        'name = "B096 Exporter"\n'
        'version = "0.1.0"\n'
        'category = "Library"\n'
        "\n[script]\n"
        f'module = "{peb.MODULE}"\n'
        'exports = ["Exports.as"]\n'
    )
    flat, nested = [], []
    for v in variants:
        line = f'    // {v.id}\n    import {v.stub} from "{peb.MODULE}";'
        (nested if v.id in NESTED_STUB else flat).append(line)
    body = "\n".join(flat)
    if nested:
        inner = "\n".join("    " + ln.lstrip() for ln in "\n".join(nested).split("\n"))
        body += f"\n    namespace {INNER} {{\n{inner}\n    }}"
    (plugin / "src" / "Exports.as").write_text(
        "// B096 probe: strict export file. Compiles into CONSUMERS only.\n"
        f"namespace {peb.MODULE} {{\n{body}\n}}\n"
    )
    bodies = "\n".join(f"// {v.id}\n{v.body}" for v in variants if v.body)
    (plugin / "src" / "Bodies.as").write_text(
        "// B096 probe: bodies, some of them inside a namespace.\n"
        f"{bodies}\n"
        "\nvoid Main() {\n"
        f'    print("{peb.TOKEN} exporter ready");\n'
        "}\n"
    )
    return plugin


peb.write_exporter = write_exporter

V = peb.V
peb.VARIANTS = [
    # The two controls, unchanged from B086 and already measured there. They
    # are here because a run without them is not evidence: an oracle that
    # cannot say BOUND cannot mean anything by UNBOUND.
    peb.VARIANTS[0],  # exact-const-ref-in  -> must be BOUND
    peb.VARIANTS[1],  # control-no-body     -> must be UNBOUND
    # B086's exact/lax pair. Carried because the harness's own selftest
    # requires it, and it is a bonus: `lax` was measured UNBOUND by B086, so
    # reproducing that here re-validates the whole harness against a known
    # row rather than trusting that it still behaves.
    peb.VARIANTS[2],  # lax-by-value-over-const-ref-in -> UNBOUND (B086)
    V(
        "ns-body-other-namespace",
        "Echo_nsflat",
        "string Echo_nsflat(const string &in a)",
        f"namespace {INNER} {{ string Echo_nsflat(const string &in a) {{ return a; }} }}",
        'string res = %(NS)s::Echo_nsflat("x");',
        "Body in a namespace whose name DIFFERS from the module. Not the "
        "shipped shape — kept as the contrast case for `ns-body-module-named`.",
        "UNKNOWN",
    ),
    V(
        "ns-body-module-named",
        "Echo_nsmod",
        "string Echo_nsmod(const string &in a)",
        # THE SHIPPED SHAPE, verified by reading tm-editor-plus-plus: its
        # module is named `Editor` AND its bodies sit in `namespace Editor`,
        # so the body's namespace EQUALS the module namespace the export file
        # already wraps the stubs in. An earlier variant here put the body in
        # a namespace with a DIFFERENT name from the module and came back
        # UNBOUND; that would have been reported as "four shipped plugins are
        # broken" off a fixture that does not match them. Kept separate so
        # the two cases cannot be confused again.
        'namespace %s { string Echo_nsmod(const string &in a) { return a; } }' % peb.MODULE,
        'string res = %(NS)s::Echo_nsmod("x");',
        "B096 claim 1, THE REAL SHAPE: body in `namespace <module>`, stub in "
        "the export file's `namespace <module>`, exactly as E++ ships it.",
        "UNKNOWN",
    ),
    V(
        "ns-both-eplusplus-shape",
        "Echo_nsboth",
        "string Echo_nsboth(const string &in a)",
        f"namespace {INNER} {{ string Echo_nsboth(const string &in a) {{ return a; }} }}",
        f'string res = %(NS)s::{INNER}::Echo_nsboth("x");',
        "THE SHIPPED SHAPE: `namespace Editor` around BOTH the stub and the "
        "body, as tm-editor-plus-plus writes it.",
        "UNKNOWN",
    ),
]

if __name__ == "__main__":
    sys.exit(peb.main())
