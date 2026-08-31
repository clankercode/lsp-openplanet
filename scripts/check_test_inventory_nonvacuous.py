#!/usr/bin/env python3
"""Prove `scripts/test_inventory.py` is non-vacuous (B085). `just test-inventory-mutants`.

A gate that cannot fail is worse than no gate, because it is believed. This
harness inflicts, on the real tree and the real built artifacts, the exact two
defects B085 exists to catch, and requires a red for each.

Three families, because they can each be vacuous in a different way:

**A — verdict guards.** Neuter one guard in a throwaway copy of
``test_inventory.py``; the script's own ``--selftest`` must go red. Copies, not
the checkout, so an interrupted run cannot leave a neutered guard behind.

**B — real defects, real artifacts.** No mutation: bump a real source file's
mtime past the binary built from it, or append a ``#[test]`` the compiled
binary cannot contain, and require the check to name it. This is the half that
proves the *measurement* works and not merely the arithmetic over it — the
selftest drives ``evaluate`` with synthetic facts and would pass happily if
``--list`` or dep-info parsing were broken.

**C — fail-closed measurement.** Neuter one instrument (the ``--list`` parse,
the dep-info parse, the ``#[test]`` scan) and require the check to go red on a
**healthy** tree. The module docstring claims a broken instrument is loud
rather than quiet; this is that claim under test. A check whose two counts both
silently read zero would agree with itself forever.

Scoring follows B077: the baseline is gated first ("BASELINE FAILED —
mutations prove nothing"), and an anchor that is missing OR ambiguous scores as
a MISS rather than being skipped, so a mutation that quietly stopped applying
cannot masquerade as one that was caught.

Family B edits files in the checkout. Every edit is restored in a ``finally``
and again from an ``atexit`` hook, and the run ends by asserting ``git
status --porcelain`` is clean for every file it touched — a restore that did
not happen is reported as a failure of this harness, never left silent.
"""

from __future__ import annotations

import atexit
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

REPO_ROOT = Path(__file__).resolve().parent.parent
SCRIPT = REPO_ROOT / "scripts" / "test_inventory.py"

# Files family B edits or moves. Registered up front so the atexit restore
# knows what to look at even if a run dies between apply and restore.
_PENDING: dict[Path, tuple[bytes, tuple[float, float]]] = {}
# `moved-away path -> where it should be put back`, for the missing-binary case.
_MOVED: dict[Path, Path] = {}


def _restore_all() -> None:
    for path, (content, times) in list(_PENDING.items()):
        try:
            path.write_bytes(content)
            os.utime(path, times)
        except OSError as exc:  # pragma: no cover - best-effort teardown
            print(f"RESTORE FAILED for {path}: {exc}", file=sys.stderr)
        _PENDING.pop(path, None)
    for stash, original in list(_MOVED.items()):
        try:
            if stash.exists():
                stash.rename(original)
        except OSError as exc:  # pragma: no cover - best-effort teardown
            print(f"RESTORE FAILED for {original}: {exc}", file=sys.stderr)
        _MOVED.pop(stash, None)


atexit.register(_restore_all)


def _snapshot(path: Path) -> None:
    st = path.stat()
    _PENDING[path] = (path.read_bytes(), (st.st_atime, st.st_mtime))


def _release(path: Path) -> None:
    entry = _PENDING.pop(path, None)
    if entry is None:
        return
    content, times = entry
    if content is not None:
        path.write_bytes(content)
    if times is not None:
        os.utime(path, times)


# --------------------------------------------------------------------------
# Loading the module (pristine or mutated) with the real repo root
# --------------------------------------------------------------------------


def load_module(path: Path):
    """Import ``test_inventory`` from ``path``, pointed at the real checkout.

    A mutated copy lives under /tmp, where its own
    ``Path(__file__).parent.parent`` would be the temp dir. The module reads
    real artifacts, so REPO_ROOT is re-pointed after import.
    """
    name = f"ti_{path.stat().st_ino}_{time.time_ns()}"
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    # `@dataclass` resolves annotations through `sys.modules[cls.__module__]`,
    # so the module has to be registered before it executes.
    sys.modules[name] = module
    spec.loader.exec_module(module)
    module.REPO_ROOT = REPO_ROOT
    return module


def verdict(module, artifacts) -> list[str]:
    """Measure the real tree with ``module`` and return its problems."""
    problems = module.verify_stripper()
    harnesses, spawned, exact = module.measure(artifacts)
    more, _total = module.evaluate(harnesses, spawned, lib_sources_are_exact=exact)
    return problems + more


# --------------------------------------------------------------------------
# Family A — neuter a verdict guard, the selftest must go red
# --------------------------------------------------------------------------

# (label, old, new)
GUARD_MUTATIONS: list[tuple[str, str, str]] = [
    (
        "content mismatch (binary count vs source count)",
        "        elif h.listed != h.declared:",
        "        elif False:",
    ),
    (
        "test harness older than its own source",
        "            if h.exe_mtime < src_mtime:",
        "            if False:",
    ),
    (
        "spawned CARGO_BIN_EXE binary older than src/",
        "            if b.mtime < src_mtime:",
        "            if False:",
    ),
    (
        "spawned CARGO_BIN_EXE binary missing",
        "        if not b.exists:",
        "        if False:",
    ),
    (
        "test harness binary missing",
        "        if not h.exists:",
        "        if False:",
    ),
    (
        "empty-artifact-set vacuity guard",
        "    if not harnesses:",
        "    if False:",
    ),
    (
        "both-instruments-read-zero vacuity guard",
        "    if harnesses and total_declared == 0 and total_listed == 0:",
        "    if False:",
    ),
    (
        "lib source-set drift guard",
        "    if not lib_sources_are_exact:",
        "    if False:",
    ),
    (
        "`--list` unreadable guard",
        "        if h.listed is None:",
        "        if False:",
    ),
    (
        "missing dep-info guard",
        "        elif h.declared is None:",
        "        elif False:",
    ),
    # The two stripper branches whose absence made earlier versions of this
    # scan silently under-read whole files.
    (
        "stripper: char literals (`b'\"'` opens a string without it)",
        '        elif c == "\'":',
        "        elif False:",
    ),
    (
        "stripper: raw strings (a fixture's embedded `#[test]` counts without it)",
        '        elif (c == "r" or (c == "b" and src[i + 1 : i + 2] == "r")) and (',
        "        elif False and (",
    ),
    (
        "stripper: line comments",
        '        elif c == "/" and src.startswith("//", i):',
        "        elif False:",
    ),
    (
        "stripper controls run at all",
        "    for label, src, expected in STRIPPER_CONTROLS:",
        "    for label, src, expected in []:",
    ),
]

# Deliberately NOT in the list above: the assertions inside ``run_selftest``
# itself. Family A's prover IS ``--selftest``, so deleting one of its
# assertions makes it pass by construction and would be scored a MISS every
# time — a category error, not a finding. Guards belong in the production path
# (``evaluate``, ``check_controls``, ``strip_literals``), which is where every
# anchor above points. The selftest's own assertions are proved instead by
# family B, which reaches the same code through real defects on disk.


def run_selftest(script: Path) -> int:
    return subprocess.run(
        [sys.executable, str(script), "--selftest"],
        capture_output=True,
        text=True,
        timeout=300,
    ).returncode


def family_a(work: Path) -> tuple[int, int]:
    target = work / "test_inventory.py"
    pristine = SCRIPT.read_text()
    target.write_text(pristine)

    if run_selftest(target) != 0:
        print("BASELINE FAILED — guard mutations prove nothing", file=sys.stderr)
        return 0, len(GUARD_MUTATIONS)
    print("  baseline: --selftest PASSES on the pristine copy")

    caught = missed = 0
    for label, old, new in GUARD_MUTATIONS:
        if old not in pristine:
            print(f"  ANCHOR MISSING: {label}", file=sys.stderr)
            missed += 1
            continue
        if pristine.count(old) != 1:
            print(
                f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}", file=sys.stderr
            )
            missed += 1
            continue
        target.write_text(pristine.replace(old, new))
        if run_selftest(target) != 0:
            print(f"  caught: {label}")
            caught += 1
        else:
            print(f"  MISSED: {label}  <-- the selftest is vacuous here", file=sys.stderr)
            missed += 1
        target.write_text(pristine)
    return caught, missed


# --------------------------------------------------------------------------
# Family B — real defects, injected into the real tree
# --------------------------------------------------------------------------

PROBE_FN = "\n#[test]\nfn b085_nonvacuity_probe_not_in_any_binary() {}\n"
N_INJECTIONS = 5


def spawned_bin_and_dependents(module, artifacts):
    """The CARGO_BIN_EXE binary to attack, and the harnesses that declare it.

    Read off the real measurement rather than hard-coded, so this harness
    cannot go on "passing" against a binary or a caller that no longer exists.
    """
    harnesses, spawned, _ = module.measure(artifacts)
    for b in spawned:
        declared_by = sorted(h.name for h in harnesses if b.name in h.spawns)
        if declared_by:
            return b.path, declared_by
    return None, []


@dataclass
class Injection:
    label: str
    apply: Callable[[], None]
    undo: Callable[[], None]
    # Every fragment must appear somewhere in the problem list.
    expect: tuple[str, ...]


def touch_forward(path: Path) -> Injection:
    """Make a source newer than the artifacts built from it.

    This is B074's mechanism reproduced exactly: the tree moved, the artifact
    did not. Nothing is rebuilt, so the injection costs nothing and the
    original mtime is restored byte-for-byte afterwards — cargo has no reason
    to rebuild once it is undone.
    """

    def apply() -> None:
        _snapshot(path)
        now = time.time()
        os.utime(path, (now, now))

    return Injection(f"source newer than artifact: {path.name}", apply, lambda: _release(path), ())


def append_test(path: Path) -> Injection:
    """Give the sources a `#[test]` the compiled binary cannot contain.

    The 52-vs-54 signature, from the other side: source declares one more test
    than the binary carries. The file's mtime is restored immediately after the
    write, so ONLY the content check can fire — proving it is the content
    instrument that caught this and not freshness riding along.
    """

    def apply() -> None:
        _snapshot(path)
        st = path.stat()
        path.write_text(path.read_text() + PROBE_FN)
        os.utime(path, (st.st_atime, st.st_mtime))

    return Injection(f"source declares a test the binary lacks: {path.name}", apply, lambda: _release(path), ())


def move_aside(path: Path) -> Injection:
    stash = path.with_name(path.name + ".b085-injected")

    def apply() -> None:
        path.rename(stash)
        _MOVED[stash] = path

    def undo() -> None:
        if stash.exists():
            stash.rename(path)
        _MOVED.pop(stash, None)

    return Injection(f"spawned binary removed: {path.name}", apply, undo, ())


def family_b(artifacts) -> tuple[int, int]:
    pristine = load_module(SCRIPT)

    baseline = verdict(pristine, artifacts)
    if baseline:
        print("BASELINE FAILED — injections prove nothing", file=sys.stderr)
        for problem in baseline:
            print(f"    {problem}", file=sys.stderr)
        return 0, N_INJECTIONS
    print("  baseline: the real tree is clean")

    lib_src = REPO_ROOT / "src" / "lib.rs"
    a_test = REPO_ROOT / "tests" / "gitignore_server_tests.rs"
    a_lib_mod = REPO_ROOT / "src" / "term.rs"
    the_bin, dependents = spawned_bin_and_dependents(pristine, artifacts)
    if the_bin is None:
        print(
            "  ANCHOR MISSING: no CARGO_BIN_EXE binary with declared dependents "
            "was measured, so blind spot 2 cannot be injected at all",
            file=sys.stderr,
        )
        return 0, N_INJECTIONS

    cases = [
        (
            touch_forward(lib_src),
            ("spawned binary", "STALE", dependents[0]),
        ),
        (
            touch_forward(a_test),
            ("gitignore_server_tests", "STALE"),
        ),
        (
            append_test(a_test),
            ("gitignore_server_tests", "CONTENT MISMATCH"),
        ),
        (
            append_test(a_lib_mod),
            ("openplanet_lsp", "CONTENT MISMATCH"),
        ),
        (
            move_aside(the_bin),
            ("is missing", dependents[0]),
        ),
    ]

    caught = missed = 0
    for injection, expect in cases:
        try:
            injection.apply()
            problems = verdict(pristine, artifacts)
        finally:
            injection.undo()
        blob = "\n".join(problems)
        absent = [fragment for fragment in expect if fragment not in blob]
        if not problems:
            print(f"  MISSED: {injection.label} — the check stayed green", file=sys.stderr)
            missed += 1
        elif absent:
            print(
                f"  MISSED: {injection.label} — flagged, but nothing mentioned "
                f"{absent}",
                file=sys.stderr,
            )
            missed += 1
        else:
            print(f"  caught: {injection.label}")
            caught += 1

    # The restore has to be checked, not assumed: this family writes into the
    # checkout, and a silent failure to undo would leave a fake test behind.
    after = verdict(pristine, artifacts)
    if after:
        print(
            "  RESTORE FAILED: the tree is not clean after the injections",
            file=sys.stderr,
        )
        for problem in after:
            print(f"    {problem}", file=sys.stderr)
        missed += 1
    else:
        print("  restore verified: the check is green again")

    dirty = subprocess.run(
        ["git", "status", "--porcelain", "--", "src", "tests"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if dirty:
        print(f"  RESTORE FAILED: git reports\n{dirty}", file=sys.stderr)
        missed += 1

    return caught, missed


# --------------------------------------------------------------------------
# Family C — a neutered instrument must go red on a HEALTHY tree
# --------------------------------------------------------------------------

INSTRUMENT_MUTATIONS: list[tuple[str, str, str]] = [
    (
        "`--list` parse reads nothing",
        'LISTED_TEST = re.compile(r": test$")',
        'LISTED_TEST = re.compile(r"(?!)")',
    ),
    (
        "`#[test]` source scan reads nothing",
        'TEST_ATTR = re.compile(r"^[ \\t]*#\\[(?:tokio::)?test[(\\]]", re.M)',
        'TEST_ATTR = re.compile(r"(?!)")',
    ),
    (
        "dep-info source list reads nothing",
        "            info.sources.add(candidate)",
        "            pass  # sources dropped",
    ),
    (
        "dep-info `# env-dep:CARGO_BIN_EXE` attribution reads nothing",
        '            info.bin_exes[env_dep["name"]] = Path(env_dep["path"])',
        "            pass  # env-dep dropped",
    ),
]


def family_c(work: Path, artifacts) -> tuple[int, int]:
    """A broken instrument must not read as a clean tree.

    The `env-dep` mutation is the one case where the tree alone cannot show
    the damage — losing the attribution does not change whether the bin is
    fresh — so that row is proved by re-running family B's stale-bin injection
    and requiring the report to still name a real dependent.
    """
    target = work / "instrument.py"
    pristine = SCRIPT.read_text()

    caught = missed = 0
    for label, old, new in INSTRUMENT_MUTATIONS:
        if old not in pristine:
            print(f"  ANCHOR MISSING: {label}", file=sys.stderr)
            missed += 1
            continue
        if pristine.count(old) != 1:
            print(
                f"  ANCHOR AMBIGUOUS ({pristine.count(old)}x): {label}", file=sys.stderr
            )
            missed += 1
            continue
        target.write_text(pristine.replace(old, new))
        module = load_module(target)

        if "env-dep" in label:
            _bin, dependents = spawned_bin_and_dependents(load_module(SCRIPT), artifacts)
            injection = touch_forward(REPO_ROOT / "src" / "lib.rs")
            try:
                injection.apply()
                problems = verdict(module, artifacts)
            finally:
                injection.undo()
            named = any(
                "spawned binary" in p and any(d in p for d in dependents)
                for p in problems
            )
            if not named:
                print(f"  caught: {label}")
                caught += 1
            else:
                print(
                    f"  MISSED: {label}  <-- attribution survived a dropped "
                    f"env-dep, so it is not being read",
                    file=sys.stderr,
                )
                missed += 1
            continue

        problems = verdict(module, artifacts)
        if problems:
            print(f"  caught: {label}")
            caught += 1
        else:
            print(
                f"  MISSED: {label}  <-- a dead instrument reported a clean tree",
                file=sys.stderr,
            )
            missed += 1
    return caught, missed


def main() -> int:
    print("==> building and enumerating once, so no injection triggers a rebuild")
    pristine = load_module(SCRIPT)
    artifacts = pristine.enumerate_artifacts()
    print(f"    {len(artifacts)} workspace artifacts\n")

    with tempfile.TemporaryDirectory(prefix="b085-mut-") as tmp:
        work = Path(tmp)
        shutil.copy2(SCRIPT, work / "test_inventory.py")

        print("==> A: verdict guards (neutered in a copy; --selftest must go red)")
        a_caught, a_missed = family_a(work)
        print()
        print("==> B: real defects injected into the real tree")
        b_caught, b_missed = family_b(artifacts)
        print()
        print("==> C: neutered instruments must go red on a healthy tree")
        c_caught, c_missed = family_c(work, artifacts)

    caught, missed = a_caught + b_caught + c_caught, a_missed + b_missed + c_missed
    print(f"\n{caught}/{caught + missed} mutations caught, {missed} missed")
    return 0 if missed == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
