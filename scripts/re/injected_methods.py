#!/usr/bin/env python3
"""Re-derive Openplanet's injected AngelScript method surface from the shipped DLL.

Openplanet adds methods to engine types that the Nadeo dump never lists, which
makes the LSP reject code the game compiles (B059, GH #21). This script recovers
the complete set from the binary and diffs it against the expected table, so an
Openplanet update that changes the surface fails loudly instead of going stale.

    python3 scripts/re/injected_methods.py [--dll PATH] [--dump PATH] [--json]

Exit 0 = surface matches EXPECTED. Exit 1 = it changed (see the diff), which
means docs/probes/openplanet-injected-methods.md and the LSP's injected-method
table both need updating.
"""
from __future__ import annotations
import argparse, hashlib, json, os, re, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from leascan import load  # noqa: E402

DEFAULT_DLL = os.path.expanduser(
    "~/.local/share/Steam/steamapps/common/Trackmania/Openplanet.dll"
)
DEFAULT_DUMP = os.path.expanduser("~/OpenplanetNext/OpenplanetNext.json")

# Mechanism B, as established 2026-08-30 against Openplanet 1.29.5 (1234ad9e).
EXPECTED = {
    "array<CGameManialinkPage@>@ GetManialinkPages()": [
        "CGameNetwork",
        "CGameCtnNetwork",
        "CGameManiaPlanetNetwork",
        "CTrackManiaNetwork",
    ],
    "uint GetCurrentEntityID()": ["CGamePlayer", "CSmPlayer"],
}

# Mechanism A: added to every registered engine class, absent from every dump.
UNIVERSAL = [
    "void MwAddRef()",
    "void MwRelease()",
    "void opCast(?&out)",
    "%s@ opImplCast()",
]

DECL = re.compile(r"^[A-Za-z_][\w:<>@&\[\]\+\*\? ]*\s[A-Za-z_]\w*\s*\(.*\)\s*(const)?$")


def dump_class_names(path):
    db = json.load(open(path))
    names = set()
    for types in db.get("ns", {}).values():
        names.update(types.keys())
    return names


# A genuine registration loads the declaration and the target type name into
# argument registers of the same call, so the two `lea`s sit a couple of
# instructions apart. Measured on 1.29.5: every real pair is 10-23 bytes apart,
# while the nearest false pair (MwId's opEquals, with CMwNod's own *type*
# registration following it) is 72. Anything past the cut is reported as
# ambiguous rather than dropped, so a layout change cannot silently lose a row.
MAX_PAIR_BYTES = 48


def derive(refs, classnames):
    """Split (declaration -> engine type) pairings into confirmed and ambiguous."""
    confirmed, ambiguous = {}, {}
    for i, (addr, s) in enumerate(refs):
        if s not in classnames or i == 0:
            continue
        prev_addr, prev = refs[i - 1]
        if not DECL.match(prev):
            continue
        bucket = confirmed if addr - prev_addr <= MAX_PAIR_BYTES else ambiguous
        bucket.setdefault(prev, []).append((s, addr, addr - prev_addr))
    return confirmed, ambiguous


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dll", default=DEFAULT_DLL)
    ap.add_argument("--dump", default=DEFAULT_DUMP)
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()

    for p in (a.dll, a.dump):
        if not os.path.exists(p):
            sys.exit(f"missing: {p}")

    data, secs, strmap, refs = load(a.dll)
    md5 = hashlib.md5(data).hexdigest()
    ver = next((s for s in strmap.values() if re.match(r"^\d+\.\d+\.\d+ \(.*\)$", s)), "?")

    classnames = dump_class_names(a.dump)
    found, ambiguous = derive(refs, classnames)
    got = {d: sorted({t for t, _, _ in ts}) for d, ts in found.items()}

    present_universal = sorted({s for _, s in refs if s in UNIVERSAL})
    all_cls_refs = [(hex(ad), s) for ad, s in refs if s in classnames]

    if a.json:
        print(json.dumps({
            "dll": a.dll, "md5": md5, "version": ver,
            "injected": got, "universal": present_universal,
            "ambiguous": {d: [[t, hex(a), g] for t, a, g in ts] for d, ts in ambiguous.items()},
            "class_name_refs": len(all_cls_refs),
        }, indent=2))
    else:
        print(f"DLL       {a.dll}")
        print(f"MD5       {md5}")
        print(f"Version   {ver}")
        print(f"Strings   {len(strmap)}   code->string refs {len(refs)}")
        print(f"\nMechanism A (universal, every engine class):")
        for u in UNIVERSAL:
            print(f"  {'OK ' if u in present_universal else 'GONE'} {u}")
        print(f"\nMechanism B (explicit per-class injections):")
        for d in sorted(got):
            print(f"  {d}")
            for t in got[d]:
                print(f"      -> {t}")
        if ambiguous:
            print("\nAmbiguous pairings (too far apart to be one call - review, do not trust):")
            for d, ts in sorted(ambiguous.items()):
                for t, ad, gap in ts:
                    print(f"  {d}  ~ {t}  ({ad:#x}, {gap} bytes apart)")
        print(f"\nCompleteness: {len(all_cls_refs)} code refs to dump-known class names")

    exp = {k: sorted(v) for k, v in EXPECTED.items()}
    if got != exp:
        print("\nSURFACE CHANGED vs EXPECTED", file=sys.stderr)
        for d in sorted(set(exp) | set(got)):
            if exp.get(d) != got.get(d):
                print(f"  {d}\n    expected: {exp.get(d)}\n    found:    {got.get(d)}", file=sys.stderr)
        print("\nUpdate docs/probes/openplanet-injected-methods.md and the LSP's"
              " injected-method table.", file=sys.stderr)
        return 1
    missing = [u for u in UNIVERSAL if u not in present_universal]
    if missing:
        print(f"\nUNIVERSAL METHODS MISSING: {missing}", file=sys.stderr)
        return 1
    print("\nMATCHES EXPECTED.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
