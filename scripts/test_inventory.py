#!/usr/bin/env python3
"""B085 — the inventory gate: the suite gate's two blind spots, closed.

    just test-inventory            # the check, standalone
    just test-inventory-selftest   # proves the verdict logic can fail
    just test-inventory-mutants    # proves it end-to-end, on real artifacts

    (`just test` runs the check as its first phase.)

Why this exists
---------------

``scripts/test-suite.py`` (B078/B082) reconciles at **binary** level: every
binary cargo enumerated produced a ``test result:`` summary. That is exactly
right for a binary that crashed or was never reached, and it is structurally
incapable of seeing either half of what produced the false B074 revert on
2026-08-31:

1. **A test vanishing INSIDE a present, healthy, summary-producing binary.**
   ``cli_check_tests`` reported a clean summary while carrying 52 issue-repro
   tests against 54 in source. Every binary reported; the total was simply two
   short. Nothing in a binary-level reconciliation can notice.

2. **A stale bin DEPENDENCY.** Many tests spawn ``CARGO_BIN_EXE_openplanet-lsp``
   as a child process. The harness can be current while the binary it measures
   is not, and then the assertion is about old code. Same fixture, same tree,
   only the two files B061 touched varying::

       openplanet-lsp built from   warning: lines on the B061 fixture
       8214488, fresh              6
       5942a75^1 (pre-B061)        13

   ``left: 13, right: 6`` — the exact reported failure — is the signature of a
   stale child binary, not of any source change. It cost a correct merge being
   wrongly reverted.

Both blind spots produce the same single symptom: **a total that fails to rise
as predicted.** Neither produces a failed test, an error, or a missing binary.
The coordinator's arithmetic caught them; the gate could not.

Where the staleness comes from here
-----------------------------------

Every lane in this repo points ``CARGO_TARGET_DIR`` at a shared cache path, and
bisects run in detached worktrees against the same warm tree. A ``git
checkout`` rewrites source files, cargo's fingerprint can still read as fresh,
and the build is skipped. This is normal practice right now, so it will recur.

The two checks
--------------

**content** — per test binary, ``<exe> --list`` count vs the number of
``#[test]`` / ``#[tokio::test]`` attributes in the sources that binary was
compiled from. The source list is not guessed: it comes from cargo's own
**dep-info** file (``<exe>.d``), which is the exact set rustc compiled,
including shared ``mod`` includes. On master this reports
``cli_check_tests binary=118 source=118``; against the 52-vs-54 tree it would
have said ``116 vs 118``.

**freshness** — per artifact, its mtime against the newest source it depends
on, again from dep-info. Applied to the test harnesses *and*, separately, to
every ``CARGO_BIN_EXE_*`` binary a harness declares — the latter is not in the
test-binary set at all, which is precisely why blind spot 2 was invisible.

Independence, and why that matters
----------------------------------

The expected per-binary count comes from a **separate** ``cargo test --no-run
--message-format=json`` enumeration plus a static read of the sources. It is
never derived from the run's own output. Using the run to check the run is the
circularity B082 fixed at the binary level; repeating it one level down would
buy nothing.

The two instruments also fail in opposite directions, which makes a broken
*measurement* loud rather than quiet:

* if the ``--list`` parse breaks, every ``bin`` count reads 0 against a nonzero
  ``source`` count — mismatch, red;
* if the dep-info parse breaks, every ``source`` count reads 0 against a
  nonzero ``bin`` count — mismatch, red.

The one way both could read zero together is an empty artifact set, so that is
checked explicitly (``no test binaries were enumerated``), as is a grand total
of zero tests.

Known limit, stated rather than papered over
--------------------------------------------

Freshness is an **mtime** comparison, so it sees "a source is newer than the
artifact built from it". That is the direction an edit or a ``git checkout``
produces, which is the observed failure. It cannot see a source restored with a
*backdated* mtime (``tar -x`` preserving times, ``rsync -a`` from an older
tree). The content check is independent of mtimes entirely and covers the
subset of that case where the test set changed.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

VERIFIED_MARKER = "INVENTORY-VERIFIED"

# `#[test]`, `#[tokio::test]`, `#[tokio::test(flavor = "...")]`, at line start.
TEST_ATTR = re.compile(r"^[ \t]*#\[(?:tokio::)?test[(\]]", re.M)
# `# env-dep:CARGO_BIN_EXE_openplanet-lsp=/abs/path/to/openplanet-lsp`
ENV_DEP_BIN = re.compile(r"^# env-dep:CARGO_BIN_EXE_(?P<name>[^=]+)=(?P<path>.+)$")
# libtest's `--list`: `some::test_name: test` (benchmarks say `: bench`).
LISTED_TEST = re.compile(r": test$")

IDENT_CHARS = frozenset(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_"
)


# --------------------------------------------------------------------------
# Reading Rust source without being fooled by text that is not code
# --------------------------------------------------------------------------


def strip_literals(src: str) -> str:
    """Blank comments and every literal form, preserving line structure.

    A port of ``src/testing.rs``'s ``env_enforcement::strip_literals``, and its
    branch list is a history rather than a spec — every one was added because
    something slipped through:

    * comments, because a doc comment quoting ``#[test]`` is prose;
    * raw strings, because fixtures in this repo embed whole Rust files in
      ``r#"..."#`` blocks, ``#[test]`` attributes included;
    * char literals, because ``b'"'`` (present in ``src/server/folding.rs``)
      read as a string opener runs to the next quote and blanks most of a file.
      That exact bug made a prototype of this check report a false
      ``2043 vs 2023`` on the lib target.

    A scan that silently reads *less* than it claims is the defect this whole
    script exists to catch, so ``verify_stripper()`` below runs a fixture
    control on every invocation.
    """
    out: list[str] = []
    i, n = 0, len(src)
    while i < n:
        c = src[i]
        if c == "\n":
            out.append("\n")
            i += 1
        elif c == "/" and src.startswith("//", i):
            while i < n and src[i] != "\n":
                i += 1
        elif c == "/" and src.startswith("/*", i):
            # Rust block comments nest.
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth += 1
                    i += 2
                elif src.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    if src[i] == "\n":
                        out.append("\n")
                    i += 1
        elif (c == "r" or (c == "b" and src[i + 1 : i + 2] == "r")) and (
            i == 0 or src[i - 1] not in IDENT_CHARS
        ):
            r = i if c == "r" else i + 1
            j = r + 1
            while j < n and src[j] == "#":
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * (j - r - 1)
                end = src.find(close, j + 1)
                end = n if end < 0 else end + len(close)
                out.append("\n" * src.count("\n", i, end))
                i = end
            else:
                out.append(c)
                i += 1
        elif c == "'":
            # A leading quote is a char literal or a lifetime, and only the
            # closing quote tells them apart.
            if src[i + 1 : i + 2] == "\\":
                k = i + 2
                while k < n and src[k] not in ("'", "\n"):
                    k += 1
                end = k if src[k : k + 1] == "'" else None
            else:
                end = i + 2 if src[i + 2 : i + 3] == "'" else None
            if end is None:
                out.append(c)  # lifetime — keep it
                i += 1
            else:
                i = end + 1  # char literal — blanked
        elif c == '"':
            i += 1
            while i < n and src[i] != '"':
                if src[i] == "\\":
                    i += 1
                elif src[i] == "\n":
                    out.append("\n")
                i += 1
            i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


# Each case is `(label, source, expected #[test] count)`. A stripper that
# under-reads scores 0 where a positive number is expected; one that
# over-reads counts the decoys. Both are failures.
STRIPPER_CONTROLS: tuple[tuple[str, str, int], ...] = (
    ("plain test", "#[test]\nfn a() {}\n", 1),
    ("tokio test", "#[tokio::test]\nfn a() {}\n", 1),
    ("tokio test with args", '#[tokio::test(flavor = "multi_thread")]\nfn a() {}\n', 1),
    ("indented", "    #[test]\n    fn a() {}\n", 1),
    ("line comment decoy", "// #[test]\nfn a() {}\n", 0),
    ("block comment decoy", "/* #[test] */\nfn a() {}\n", 0),
    ("nested block comment decoy", "/* /* #[test] */ #[test] */\n", 0),
    ("raw string decoy", 'let s = r#"\n#[test]\nfn x() {}\n"#;\n', 0),
    ("byte raw string decoy", 'let s = br##"\n#[test]\n"##;\n', 0),
    ("plain string decoy", 'let s = "#[test]";\n', 0),
    # A decoy has to DISCRIMINATE, not merely come out at the expected number.
    # `r#"..."#` with an even number of quotes inside reads as 0 whether or not
    # raw strings are handled at all, because the plain-string branch happens
    # to blank the same span; the odd quote below is what makes the branch
    # load-bearing. Same for the comment: an unbalanced quote inside it
    # swallows the rest of the file once comments stop being skipped.
    (
        "raw string with an unpaired quote inside",
        'let s = r#"\nhe said "hi\n#[test]\nfn x() {}\n"#;\n',
        0,
    ),
    ("comment with an unpaired quote inside", '// a "quote\n#[test]\nfn a() {}\n', 1),
    # The char-literal cases are the ones that mattered: a `'"'` read as a
    # string opener swallows everything to the next quote.
    ("char double-quote does not swallow", "let c = '\"';\n#[test]\nfn a() {}\n", 1),
    ("escaped char", "let c = '\\'';\n#[test]\nfn a() {}\n", 1),
    ("escaped newline char", "let c = '\\n';\n#[test]\nfn a() {}\n", 1),
    ("brace char literal", "let c = '{';\n#[test]\nfn a() {}\n", 1),
    ("lifetime is not a char", "fn f<'a>(x: &'a str) {}\n#[test]\nfn a() {}\n", 1),
    ("multibyte char literal", "let c = 'é';\n#[test]\nfn a() {}\n", 1),
    # A decoy AFTER a tricky literal: proves the scan resumed, rather than
    # running to end-of-file in a swallowed state and reporting a clean 0.
    ("resumes after char literal", "let c = '\"';\n// #[test]\n#[test]\nfn a() {}\n", 1),
    ("escaped quote inside string", 'let s = "a\\"b";\n#[test]\nfn a() {}\n', 1),
)


def check_controls(strip=strip_literals) -> list[str]:
    """Run the fixture controls against ``strip``; return what it got wrong.

    Parameterised on the stripper so the selftest can point it at a broken one
    and require a red — a control suite that passes with the thing under test
    removed is testing nothing, and this suite is what the whole source count
    rests on.
    """
    problems = []
    checked = 0
    for label, src, expected in STRIPPER_CONTROLS:
        checked += 1
        got = len(TEST_ATTR.findall(strip(src)))
        if got != expected:
            problems.append(
                f"source scanner control `{label}`: expected {expected} "
                f"`#[test]`, counted {got}"
            )
    # The loop not running is indistinguishable from every case passing.
    if checked == 0 or checked != len(STRIPPER_CONTROLS):
        problems.append(
            f"the source-scanner controls did not run: {checked} of "
            f"{len(STRIPPER_CONTROLS)} cases were evaluated"
        )
    return problems


def verify_stripper() -> list[str]:
    """Control for the source scanner itself. Runs on every invocation.

    Without this, a stripper that swallowed whole files would report
    ``source=0`` — and ``source=0`` against ``binary=0`` on an empty artifact
    set is a clean pass. The count this check compares against is only as
    trustworthy as the scan that produced it.
    """
    return check_controls()


def count_tests_in_sources(paths: list[Path]) -> int:
    total = 0
    for path in paths:
        try:
            text = path.read_text(errors="replace")
        except OSError:
            continue
        total += len(TEST_ATTR.findall(strip_literals(text)))
    return total


# --------------------------------------------------------------------------
# Reading cargo's dep-info
# --------------------------------------------------------------------------


@dataclass
class DepInfo:
    """One parsed ``<artifact>.d`` file."""

    sources: set[Path] = field(default_factory=set)
    # CARGO_BIN_EXE_<name> -> path, straight from rustc's env-dep records.
    bin_exes: dict[str, Path] = field(default_factory=dict)


def parse_dep_info(path: Path, root: Path) -> DepInfo:
    """Parse a cargo dep-info file: Makefile rules plus ``# env-dep:`` lines.

    The ``env-dep`` lines are what make blind spot 2 checkable without
    grepping test sources for ``env!``: rustc records every environment
    variable the crate read at compile time, so a harness that spawns
    ``CARGO_BIN_EXE_openplanet-lsp`` *declares itself*, and declares the exact
    path it was handed.
    """
    info = DepInfo()
    try:
        text = path.read_text(errors="replace")
    except OSError:
        return info
    for line in text.splitlines():
        env_dep = ENV_DEP_BIN.match(line)
        if env_dep:
            info.bin_exes[env_dep["name"]] = Path(env_dep["path"])
            continue
        if line.startswith("#") or ":" not in line:
            continue
        _target, _, rhs = line.partition(":")
        for token in re.split(r"(?<!\\) ", rhs.strip()):
            token = token.replace("\\ ", " ").strip()
            if not token:
                continue
            candidate = Path(token)
            if not candidate.is_absolute():
                candidate = root / candidate
            info.sources.add(candidate)
    return info


def newest(paths) -> tuple[Path, float] | None:
    """The most recently modified existing path, or None if none exist."""
    best: tuple[Path, float] | None = None
    for path in paths:
        try:
            mtime = path.stat().st_mtime
        except OSError:
            continue
        if best is None or mtime > best[1]:
            best = (path, mtime)
    return best


# --------------------------------------------------------------------------
# The measured facts, and the verdict over them
# --------------------------------------------------------------------------


@dataclass
class Harness:
    """One test binary, as measured."""

    name: str
    exe: Path
    exists: bool
    exe_mtime: float | None
    listed: int | None  # from `<exe> --list`; None if it could not be run
    declared: int | None  # from `#[test]` in its dep-info sources
    newest_source: tuple[Path, float] | None
    spawns: list[str] = field(default_factory=list)


@dataclass
class SpawnedBin:
    """A `CARGO_BIN_EXE_*` binary that test harnesses run as a child."""

    name: str
    path: Path
    exists: bool
    mtime: float | None
    newest_source: tuple[Path, float] | None
    dependents: list[str] = field(default_factory=list)


def evaluate(
    harnesses: list[Harness],
    spawned: list[SpawnedBin],
    lib_sources_are_exact: bool = True,
) -> tuple[list[str], int]:
    """The verdict. Pure, so the selftest drives this exact code.

    Returns ``(problems, total_declared_tests)``. Empty problems means the
    inventory is trustworthy.
    """
    problems: list[str] = []

    # An empty set passes every per-item check below by vacuity, and an
    # inventory over nothing is the one way both instruments can read zero
    # together. Refuse it explicitly.
    if not harnesses:
        problems.append(
            "no test binaries were enumerated — the inventory would pass by "
            "vacuity, so this is a failure, not an empty success"
        )

    total_declared = sum(h.declared for h in harnesses if h.declared is not None)
    total_listed = sum(h.listed for h in harnesses if h.listed is not None)
    if harnesses and total_declared == 0 and total_listed == 0:
        problems.append(
            "both instruments read zero tests across every binary — the "
            "measurement is broken, not the tree"
        )

    for h in sorted(harnesses, key=lambda h: h.name):
        if not h.exists:
            problems.append(f"`{h.name}`: cargo named {h.exe} but it does not exist")
            continue
        if h.listed is None:
            problems.append(f"`{h.name}`: `--list` could not be read from {h.exe}")
        elif h.declared is None:
            problems.append(f"`{h.name}`: no dep-info, so its sources are unknown")
        elif h.listed != h.declared:
            direction = "fewer" if h.listed < h.declared else "more"
            problems.append(
                f"`{h.name}`: CONTENT MISMATCH — the binary carries {h.listed} "
                f"test(s), its sources declare {h.declared} "
                f"({abs(h.declared - h.listed)} {direction} in the binary). A "
                f"binary-level gate sees a clean summary here."
            )
        if h.newest_source and h.exe_mtime is not None:
            src, src_mtime = h.newest_source
            if h.exe_mtime < src_mtime:
                problems.append(
                    f"`{h.name}`: STALE — built {_ago(src_mtime - h.exe_mtime)} "
                    f"before its newest source {_rel(src)}. Its assertions are "
                    f"about older code than the tree holds."
                )

    for b in sorted(spawned, key=lambda b: b.name):
        who = ", ".join(sorted(b.dependents)) or "(no declared dependents)"
        if not b.exists:
            problems.append(
                f"spawned binary `{b.name}` is missing at {b.path}, but "
                f"{who} spawn it as a child process"
            )
            continue
        if b.newest_source and b.mtime is not None:
            src, src_mtime = b.newest_source
            if b.mtime < src_mtime:
                problems.append(
                    f"spawned binary `{b.name}`: STALE — built "
                    f"{_ago(src_mtime - b.mtime)} before its newest source "
                    f"{_rel(src)}. It is NOT in the test-binary set, so the "
                    f"suite gate cannot see it, and every assertion made by "
                    f"{who} is about the older binary."
                )

    if not lib_sources_are_exact:
        problems.append(
            "the lib source set was approximated by a glob and no longer "
            "matches cargo's dep-info — the freshness check's inputs have "
            "drifted from what cargo actually compiles"
        )

    return problems, total_declared


def _rel(path: Path) -> str:
    try:
        return str(path.relative_to(REPO_ROOT))
    except ValueError:
        return str(path)


def _ago(seconds: float) -> str:
    if seconds < 90:
        return f"{seconds:.0f}s"
    if seconds < 5400:
        return f"{seconds / 60:.0f}m"
    if seconds < 172800:
        return f"{seconds / 3600:.1f}h"
    return f"{seconds / 86400:.1f}d"


# --------------------------------------------------------------------------
# Measurement
# --------------------------------------------------------------------------


def enumerate_artifacts(cargo_args: list[str] | None = None) -> list[dict]:
    """Build, and return cargo's own artifact records for this workspace.

    A SEPARATE cargo invocation, deliberately — see the module docstring. The
    non-test bin targets are kept as well as the test harnesses: the bin a
    test spawns is the artifact blind spot 2 lives in, and it is not a test
    binary.
    """
    proc = subprocess.run(
        [
            "cargo",
            "test",
            "--locked",
            "--no-run",
            "--message-format=json",
            *(cargo_args or []),
        ],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        sys.exit(f"error: `cargo test --no-run` failed (exit {proc.returncode})")

    artifacts = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact" or not msg.get("executable"):
            continue
        manifest = Path(msg.get("manifest_path", "")).resolve()
        if REPO_ROOT not in manifest.parents:
            continue
        artifacts.append(msg)
    return artifacts


def lib_sources(artifacts: list[dict]) -> tuple[set[Path], bool]:
    """The sources compiled into the crate's lib, and whether that is exact.

    Every target in this crate links the lib, so a change under ``src/`` makes
    every artifact stale — but only ``src/main.rs`` belongs to the bin and not
    the lib, so a plain ``src/**/*.rs`` glob would flag every integration test
    as stale the moment someone edits ``main.rs``.

    Cargo's dep-info for the lib target is the exact set. When a filtered run
    did not enumerate the lib, fall back to the glob minus ``src/main.rs`` —
    and when the exact set IS available, compare the two, so the fallback
    cannot silently rot into being wrong.
    """
    exact: set[Path] | None = None
    for msg in artifacts:
        if "lib" in msg["target"]["kind"]:
            exe = Path(msg["executable"])
            info = parse_dep_info(exe.with_name(exe.name + ".d"), REPO_ROOT)
            found = {p for p in info.sources if p.suffix == ".rs"}
            if found:
                exact = found
            break

    glob = {p.resolve() for p in (REPO_ROOT / "src").rglob("*.rs")}
    glob.discard((REPO_ROOT / "src" / "main.rs").resolve())

    if exact is None:
        return glob, True  # nothing to compare against; not a drift claim
    return exact, {p.resolve() for p in exact} == glob


def measure(artifacts: list[dict]) -> tuple[list[Harness], list[SpawnedBin], bool]:
    lib_srcs, exact = lib_sources(artifacts)

    harnesses: list[Harness] = []
    # path -> SpawnedBin, so two harnesses naming the same bin share one entry.
    spawned: dict[Path, SpawnedBin] = {}

    for msg in artifacts:
        target = msg["target"]
        exe = Path(msg["executable"])
        root_src = Path(target["src_path"])
        info = parse_dep_info(exe.with_name(exe.name + ".d"), REPO_ROOT)

        # Every target here links the lib, so the lib's sources are part of
        # every artifact's freshness set. `root_src` covers the uplifted bin,
        # whose dep-info does not sit beside it.
        freshness_srcs = info.sources | lib_srcs | {root_src}
        exe_mtime = exe.stat().st_mtime if exe.exists() else None

        if not msg["profile"]["test"]:
            # A non-test bin. Interesting only as something a harness spawns;
            # recorded here so its freshness is measured even when no harness
            # happens to declare it this run.
            spawned.setdefault(
                exe.resolve(),
                SpawnedBin(
                    name=target["name"],
                    path=exe,
                    exists=exe.exists(),
                    mtime=exe_mtime,
                    newest_source=newest(freshness_srcs),
                ),
            )
            continue

        listed: int | None = None
        if exe.exists():
            proc = subprocess.run(
                [str(exe), "--list"], capture_output=True, text=True, check=False
            )
            if proc.returncode == 0:
                listed = sum(
                    1 for line in proc.stdout.splitlines() if LISTED_TEST.search(line)
                )

        # Only the sources rustc actually compiled into THIS binary, so a
        # shared `mod` included by two harnesses counts for both, and a
        # `tests/*.rs` belonging to another harness counts for neither.
        own_srcs = sorted(p for p in info.sources if p.suffix == ".rs")
        declared = count_tests_in_sources(own_srcs) if own_srcs else None

        harnesses.append(
            Harness(
                name=target["name"],
                exe=exe,
                exists=exe.exists(),
                exe_mtime=exe_mtime,
                listed=listed,
                declared=declared,
                newest_source=newest(freshness_srcs),
                spawns=sorted(info.bin_exes),
            )
        )

        for bin_name, bin_path in info.bin_exes.items():
            key = bin_path.resolve()
            entry = spawned.get(key)
            if entry is None:
                entry = SpawnedBin(
                    name=bin_name,
                    path=bin_path,
                    exists=bin_path.exists(),
                    mtime=bin_path.stat().st_mtime if bin_path.exists() else None,
                    newest_source=newest(lib_srcs | {REPO_ROOT / "src" / "main.rs"}),
                )
                spawned[key] = entry
            entry.dependents.append(target["name"])

    return harnesses, list(spawned.values()), exact


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def report(
    harnesses: list[Harness],
    spawned: list[SpawnedBin],
    problems: list[str],
    total: int,
    verbose: bool,
) -> str:
    lines: list[str] = []
    if verbose:
        for h in sorted(harnesses, key=lambda h: h.name):
            spawns = f"  spawns {', '.join(h.spawns)}" if h.spawns else ""
            lines.append(
                f"  {h.name:<30} binary={_n(h.listed):<6} source={_n(h.declared):<6}"
                f"{spawns}"
            )
        for b in sorted(spawned, key=lambda b: b.name):
            lines.append(
                f"  [bin] {b.name:<24} spawned by {len(b.dependents)} harness(es)"
            )
        lines.append("")

    if problems:
        lines.append(
            f"FAIL: the test inventory is not trustworthy "
            f"({len(harnesses)} binaries checked)."
        )
        for problem in problems:
            lines.append(f"  - {problem}")
        lines.append("")
        lines.append(
            "A binary-level gate cannot see any of the above: every binary is\n"
            "present and reports a clean summary. The total is simply wrong.\n"
            "Rebuild from scratch (`cargo clean -p openplanet-lsp`, or an\n"
            "unshared CARGO_TARGET_DIR) before believing any number. See B085."
        )
        return "\n".join(lines)

    lines.append(
        f"{VERIFIED_MARKER}: {len(harnesses)} binaries, {total} tests reconciled "
        f"against source; {len(spawned)} spawned binary/binaries fresh"
    )
    return "\n".join(lines)


def _n(value: int | None) -> str:
    return "?" if value is None else str(value)


def check(cargo_args: list[str] | None = None, verbose: bool = False) -> tuple[int, str]:
    """Run the whole check. Returns ``(status, report)``; 0 means trustworthy."""
    problems = verify_stripper()
    if problems:
        detail = "\n".join(f"  - {p}" for p in problems)
        return 1, (
            "FAIL: the source scanner is broken, so no count it produces means "
            f"anything.\n{detail}"
        )

    artifacts = enumerate_artifacts(cargo_args)
    harnesses, spawned, exact = measure(artifacts)
    problems, total = evaluate(harnesses, spawned, lib_sources_are_exact=exact)
    return (1 if problems else 0), report(harnesses, spawned, problems, total, verbose)


# --------------------------------------------------------------------------
# Selftest: the verdict logic must be able to fail
# --------------------------------------------------------------------------


def _healthy() -> tuple[list[Harness], list[SpawnedBin]]:
    """A synthetic inventory with nothing wrong with it."""
    old, new = 1000.0, 900.0  # artifact at 1000, sources at 900: fresh
    bin_path = Path("/t/debug/openplanet-lsp")
    harnesses = [
        Harness(
            name="cli_check_tests",
            exe=Path("/t/deps/cli_check_tests-0"),
            exists=True,
            exe_mtime=old,
            listed=118,
            declared=118,
            newest_source=(Path("/t/tests/cli_check_tests.rs"), new),
            spawns=["openplanet-lsp"],
        ),
        Harness(
            name="openplanet_lsp",
            exe=Path("/t/deps/openplanet_lsp-0"),
            exists=True,
            exe_mtime=old,
            listed=2053,
            declared=2053,
            newest_source=(Path("/t/src/lib.rs"), new),
        ),
    ]
    spawned = [
        SpawnedBin(
            name="openplanet-lsp",
            path=bin_path,
            exists=True,
            mtime=old,
            newest_source=(Path("/t/src/lib.rs"), new),
            dependents=["cli_check_tests"],
        )
    ]
    return harnesses, spawned


def run_selftest() -> int:
    """Inject each defect the check claims to catch; require a red for every one.

    This exercises ``evaluate`` — the same function the real run uses — with
    synthetic facts, so it is fast and needs no build. The end-to-end proof,
    against real artifacts on disk, is
    ``scripts/check_test_inventory_nonvacuous.py``.
    """
    failures: list[str] = []

    stripper = verify_stripper()
    if stripper:
        failures.extend(stripper)
    else:
        print(f"  ok: source scanner controls ({len(STRIPPER_CONTROLS)} cases)")
    # Control of the control: with no stripping at all, the decoys must be
    # miscounted. If they are not, the suite is not exercising the stripper.
    if not check_controls(lambda src: src):
        failures.append(
            "MISSED: the source-scanner controls pass with the stripper "
            "removed entirely — they do not exercise it"
        )
    else:
        print("  caught: controls fail when the stripper is removed")

    # Baseline first, as in B077: if a healthy inventory does not pass, every
    # red below could be the harness rather than the defect.
    base_problems, base_total = evaluate(*_healthy())
    if base_problems:
        print("BASELINE FAILED — the injections below prove nothing", file=sys.stderr)
        for problem in base_problems:
            print(f"  - {problem}", file=sys.stderr)
        return 2
    if base_total != 2171:
        failures.append(f"baseline total is {base_total}, expected 2171")
    print("  ok: baseline healthy inventory passes")

    def inject(label: str, mutate, expect: str) -> None:
        harnesses, spawned = _healthy()
        mutate(harnesses, spawned)
        problems, _ = evaluate(harnesses, spawned)
        if not problems:
            failures.append(f"MISSED: {label} — the check accepted it")
        elif not any(expect in p for p in problems):
            failures.append(
                f"MISSED: {label} — flagged, but no problem mentions {expect!r}: "
                f"{problems}"
            )
        else:
            print(f"  caught: {label}")

    # Blind spot 1: a test vanishes inside a healthy, summary-producing binary.
    inject(
        "a test vanished inside a healthy binary (52 vs 54)",
        lambda h, s: setattr(h[0], "listed", 116),
        "CONTENT MISMATCH",
    )
    inject(
        "the binary carries a test the sources do not declare",
        lambda h, s: setattr(h[0], "listed", 119),
        "CONTENT MISMATCH",
    )
    # Blind spot 2: the spawned bin is stale. This is the B074 signature.
    inject(
        "the spawned CARGO_BIN_EXE binary is older than src/",
        lambda h, s: setattr(s[0], "mtime", 800.0),
        "spawned binary",
    )
    inject(
        "the spawned CARGO_BIN_EXE binary is missing",
        lambda h, s: (setattr(s[0], "exists", False)),
        "is missing",
    )
    # The named dependents matter: a stale bin report that does not say which
    # harnesses were measuring against it leaves the reader nowhere to go.
    harnesses, spawned = _healthy()
    spawned[0].mtime = 800.0
    problems, _ = evaluate(harnesses, spawned)
    if not any("cli_check_tests" in p for p in problems):
        failures.append(
            "MISSED: a stale spawned bin was reported without naming the "
            "harnesses that spawn it"
        )
    else:
        print("  caught: stale spawned bin names its dependents")

    inject(
        "a test harness is older than its own source",
        lambda h, s: setattr(h[0], "exe_mtime", 800.0),
        "STALE",
    )
    inject(
        "a test harness binary cargo named does not exist",
        lambda h, s: setattr(h[0], "exists", False),
        "does not exist",
    )
    inject(
        "`--list` could not be read",
        lambda h, s: setattr(h[0], "listed", None),
        "--list",
    )
    inject(
        "dep-info was missing, so sources are unknown",
        lambda h, s: setattr(h[0], "declared", None),
        "no dep-info",
    )
    drift_problems, _ = evaluate(*_healthy(), lib_sources_are_exact=False)
    if not any("drifted" in p for p in drift_problems):
        failures.append("MISSED: lib source-set drift was not reported")
    else:
        print("  caught: lib source-set drift")

    # Vacuity guards: the two ways this check could pass while measuring
    # nothing at all.
    empty_problems, _ = evaluate([], [])
    if not empty_problems:
        failures.append(
            "MISSED: an empty artifact set passed — the check would be vacuous "
            "whenever enumeration returns nothing"
        )
    else:
        print("  caught: empty artifact set")

    harnesses, spawned = _healthy()
    for h in harnesses:
        h.listed = h.declared = 0
    zero_problems, _ = evaluate(harnesses, spawned)
    if not any("read zero" in p for p in zero_problems):
        failures.append(
            "MISSED: both instruments reading zero passed — a broken "
            "measurement would agree with itself"
        )
    else:
        print("  caught: both instruments reading zero")

    if failures:
        print("\nFAIL: test-inventory selftest", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(
        "\nPASS: every defect the inventory check claims to catch was injected\n"
        "and produced a red, including both ways it could pass by vacuity."
    )
    return 0


USAGE = """usage: test_inventory.py [--selftest] [-v] [cargo args...]

  --selftest   inject each defect the check claims to catch; require a red
  -v           list every binary with its binary-vs-source counts
  anything else is forwarded to `cargo test --locked --no-run`
"""


def main() -> int:
    argv = sys.argv[1:]
    if "-h" in argv or "--help" in argv:
        print(USAGE)
        return 0
    if "--selftest" in argv:
        if argv != ["--selftest"]:
            sys.exit("error: --selftest takes no other arguments")
        return run_selftest()
    verbose = "-v" in argv
    argv = [a for a in argv if a != "-v"]
    status, text = check(argv, verbose=verbose)
    print(text, file=sys.stderr if status else sys.stdout)
    return status


if __name__ == "__main__":
    sys.exit(main())
