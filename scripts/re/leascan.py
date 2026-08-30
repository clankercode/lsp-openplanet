#!/usr/bin/env python3
"""Recover code->string references from a PE32+ binary without a disassembler.

Openplanet registers its AngelScript surface with plain C string literals passed
to RegisterObjectMethod/RegisterGlobalFunction. Those literals are not reachable
through a pointer table -- each call site loads one with `lea reg, [rip+disp32]`.
This module indexes the strings by VMA and resolves every such lea, giving
(code_vma, string) pairs.

Used as a library by injected_methods.py; run directly to dump all references.
"""
from __future__ import annotations
import struct, sys


def pe_sections(data: bytes):
    """Yield (name, vma, file_off, raw_size) for each section. Parses the real
    headers so this keeps working across Openplanet updates."""
    if data[:2] != b"MZ":
        raise ValueError("not a PE file (no MZ)")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe : pe + 4] != b"PE\0\0":
        raise ValueError("not a PE file (no PE signature)")
    nsec = struct.unpack_from("<H", data, pe + 6)[0]
    opt_size = struct.unpack_from("<H", data, pe + 20)[0]
    magic = struct.unpack_from("<H", data, pe + 24)[0]
    if magic != 0x20B:
        raise ValueError(f"expected PE32+ (0x20b), got {magic:#x}")
    image_base = struct.unpack_from("<Q", data, pe + 24 + 24)[0]
    tab = pe + 24 + opt_size
    for i in range(nsec):
        e = tab + i * 40
        name = data[e : e + 8].rstrip(b"\0").decode("ascii", "replace")
        vsize, vaddr, rawsize, rawptr = struct.unpack_from("<IIII", data, e + 8)
        yield name, image_base + vaddr, rawptr, min(rawsize, vsize or rawsize)


def index_strings(data, secs, min_len=3):
    """VMA -> str for NUL-terminated printable ASCII in data sections."""
    out = {}
    for name, vma, off, size in secs:
        if name == ".text":
            continue
        i, end = off, off + size
        while i < end:
            j = i
            while j < end and 32 <= data[j] < 127:
                j += 1
            if j - i >= min_len and j < end and data[j] == 0:
                out[vma + (i - off)] = data[i:j].decode("ascii")
            i = j + 1 if j < end else end
    return out


def scan_refs(data, secs, strmap):
    """(code_vma, string) for every `lea r64,[rip+disp32]` landing on a string."""
    text = next((s for s in secs if s[0] == ".text"), None)
    if text is None:
        raise ValueError("no .text section")
    _, tvma, toff, tsize = text
    refs = []
    i, end = toff, toff + tsize - 7
    while i < end:
        b = data[i]
        # REX.W (48) or REX.WR (4C), 8D = lea, modrm mod=00 rm=101 -> RIP-relative
        if (b == 0x48 or b == 0x4C) and data[i + 1] == 0x8D and (data[i + 2] & 0xC7) == 0x05:
            disp = struct.unpack_from("<i", data, i + 3)[0]
            s = strmap.get(tvma + (i + 7 - toff) + disp)
            if s is not None:
                refs.append((tvma + (i - toff), s))
            i += 7
            continue
        i += 1
    return refs


def load(path):
    data = open(path, "rb").read()
    secs = list(pe_sections(data))
    strmap = index_strings(data, secs)
    return data, secs, strmap, scan_refs(data, secs, strmap)


if __name__ == "__main__":
    p = sys.argv[1] if len(sys.argv) > 1 else None
    if not p:
        sys.exit("usage: leascan.py <path-to.dll>")
    _, secs, strmap, refs = load(p)
    print(f"# sections={len(secs)} strings={len(strmap)} refs={len(refs)}", file=sys.stderr)
    for a, s in refs:
        print(f"{a:#x}\t{s!r}")
