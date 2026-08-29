#!/usr/bin/env python3
"""B045 — hermetic test gate: run the suite with an isolated HOME.

Since B019 the ``DEPENDENCY_<ID>`` define is gated on a dependency actually
RESOLVING, so a test that reaches this machine's real Openplanet install passes
for its author and fails for everyone else — including CI — and the failure
reads as flakiness rather than as a hermeticity bug. This runs every test
binary with ``HOME`` / ``USERPROFILE`` pointed at a fresh empty directory, so
such a test goes red where it is written.

    just test-hermetic            # the gate
    just test-hermetic-selftest   # proves the gate can fail

This is an ADDITIONAL gate, never a replacement for ``just test`` — the normal
run is faster and is what developers iterate against.

Why not just ``HOME=$(mktemp -d) cargo test``: rustup resolves the toolchain
through the real ``HOME``, so cargo cannot run under a fake one. Hence
``cargo test --no-run`` to build and enumerate, then each binary executed
directly with the isolated environment (the procedure B019 validated by hand).

Environment the child binaries see
----------------------------------
``HOME`` and ``USERPROFILE`` are set to a fresh empty temp dir, one per binary.

Two further groups are REMOVED, because HOME isolation alone does not stop
them pointing back at this machine:

* **Config-path surfaces** — ``XDG_CONFIG_HOME`` and ``APPDATA`` both win over
  ``HOME`` in ``config_dir()`` (``src/update.rs``), so an inherited value would
  route straight around the sandbox.
* **Every ``OPENPLANET*`` variable.** These are the suite's opt-in escape
  hatches: ``OPENPLANET_REAL_PLUGINS_DIR`` is passed verbatim as
  ``--plugins-dir`` (``tests/integration_tests.rs``), and
  ``OPENPLANET_PLUGINS`` / ``OPENPLANET_PLUGINS_DIR`` /
  ``OPENPLANET_TYPEDB_DIR`` / ``OPENPLANET_DEBUG_FILE`` name real corpora on
  disk. A developer who exports any of them — which is exactly what they are
  for — would otherwise hand the "hermetic" run a real install back. The whole
  prefix is scrubbed rather than a fixed list so a future opt-in is covered the
  day it is added. Tests gated on them take their documented skip path here;
  run them under ``just test`` when you want them.
* ``DUMP_FILE`` / ``DUMP_UNKNOWN_TYPES`` — same shape, without the prefix.

Everything else is inherited, so ``RUST_TEST_THREADS``, ``CARGO_BUILD_JOBS``
and friends still work.

What this gate does NOT catch
-----------------------------
* **Absolute paths outside HOME.** Isolation is a runtime env change; a
  hardcoded ``--plugins-dir /opt/...`` or a compile-time ``CARGO_MANIFEST_DIR``
  path is untouched. What makes the gate sufficient today is a separate,
  unenforced property: every ``--plugins-dir`` in the suite points inside a
  temp fixture dir.
* **Tests that depend on something being ABSENT.** "No install" is the
  environment under test, so a test needing a plugin to be missing is green
  here and red on a developer's box — the failure inverted, not removed.
* **Doctests.** ``cargo test --no-run`` does not build them, so they are not
  part of this run. ``just test`` covers them under the real HOME.
* **Non-HOME machine state** — installed system packages, network, clock,
  locale.

Both blind spots above are kept executable as ``uncaught_*`` canaries in
``tests/hermeticity_canary.rs`` rather than living only in this comment.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Inherited env that would defeat HOME isolation. See the module docstring.
SCRUBBED_ENV = ("XDG_CONFIG_HOME", "APPDATA", "DUMP_FILE", "DUMP_UNKNOWN_TYPES")
SCRUBBED_PREFIXES = ("OPENPLANET",)

CANARY_TARGET = "hermeticity_canary"

# The canary inventory, pinned. Deriving it from `--list` alone would let a
# deleted or renamed canary pass silently, which is the same "gate that cannot
# fail" failure B045 exists to prevent — so the listed set must EQUAL this one.
# Adding a canary means adding it here; that is the point.
REQUIRED_CANARIES = {
    # must FAIL under isolation — each depends on a real HOME
    "caught_auto_detect_finds_a_local_openplanet_install": "FAILED",
    "caught_home_plugins_dir_lists_installed_plugins": "FAILED",
    "caught_user_config_dir_resolves_under_a_real_home": "FAILED",
    "caught_userprofile_points_at_an_openplanet_install": "FAILED",
    # must PASS under isolation — documented blind spots
    "uncaught_absolute_path_outside_home": "ok",
    "uncaught_dependency_on_a_plugin_being_absent": "ok",
}

# libtest's stable-channel result lines: `test some::name ... ok`. `--format
# json` is nightly-only, so this is the only machine-readable surface there is.
# The cargo-side enumeration below uses `--message-format=json` precisely
# because it has a stable structured form and this one does not.
TEST_RESULT_LINE = re.compile(r"^test (?P<name>\S+) \.\.\. (?P<status>.+)$")
SUMMARY_LINE = re.compile(
    r"^test result: \S+\. (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
    r"(?P<ignored>\d+) ignored"
)


def hermetic_env(home: Path) -> dict[str, str]:
    """A copy of the ambient env with every HOME-derived surface isolated."""
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in SCRUBBED_ENV and not key.startswith(SCRUBBED_PREFIXES)
    }
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    return env


def build_and_enumerate(quiet: bool = False) -> list[tuple[str, str]]:
    """Build the test binaries; return ``(target name, executable path)``.

    Uses ``--message-format=json`` and selects artifacts by ``profile.test``
    rather than scraping ``Executable ...`` lines out of cargo's human output,
    which has no compatibility guarantee.
    """
    if not quiet:
        print("==> cargo test --locked --no-run", flush=True)
    proc = subprocess.run(
        ["cargo", "test", "--locked", "--no-run", "--message-format=json"],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        sys.exit(f"error: `cargo test --no-run` failed (exit {proc.returncode})")

    found: dict[str, str] = {}
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        executable = msg.get("executable")
        if not executable or not msg.get("profile", {}).get("test"):
            continue
        # Workspace targets only — never a dependency's own test harness.
        manifest = Path(msg.get("manifest_path", "")).resolve()
        if REPO_ROOT not in manifest.parents:
            continue
        found[executable] = msg.get("target", {}).get("name", "?")

    if not found:
        sys.exit("error: no test binaries found in cargo's JSON output")
    return [(name, exe) for exe, name in found.items()]


def run_gate() -> int:
    binaries = build_and_enumerate()
    print(f"==> {len(binaries)} test binaries, isolated HOME per binary\n", flush=True)

    total_passed = total_failed = total_ignored = 0
    failed_binaries: list[str] = []

    for name, exe in sorted(binaries):
        with tempfile.TemporaryDirectory(prefix="openplanet-lsp-hermetic-") as home:
            proc = subprocess.run(
                [exe],
                cwd=REPO_ROOT,
                env=hermetic_env(Path(home)),
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
        summaries = [ln for ln in proc.stdout.splitlines() if ln.startswith("test result:")]
        for match in (SUMMARY_LINE.match(ln) for ln in summaries):
            if match is None:
                # Counts are cosmetic; the per-binary exit code below is what
                # the gate actually rules on, so never fail the run over this.
                print(f"warning: unparsable libtest summary in `{name}`", file=sys.stderr)
                continue
            total_passed += int(match["passed"])
            total_failed += int(match["failed"])
            total_ignored += int(match["ignored"])

        # A binary that exits 0 without a libtest summary never ran its tests
        # (killed, or a harness that is not libtest). Treating that as a pass
        # is the silent-green failure this gate exists to prevent.
        ran = bool(summaries)
        status = "ok" if proc.returncode == 0 and ran else "FAILED"
        detail = summaries[0] if ran else f"(no libtest summary, exit {proc.returncode})"
        print(f"  {status:6}  {name:32}  {detail}", flush=True)
        if status != "ok":
            failed_binaries.append(name)
            print(proc.stdout, file=sys.stderr, flush=True)

    print(
        f"\n==> {len(binaries)} binaries, {total_passed} passed, "
        f"{total_failed} failed, {total_ignored} ignored"
    )
    if failed_binaries:
        print(
            "\nFAIL: hermetic run is red in: " + ", ".join(failed_binaries) + "\n"
            "A test that passes under `just test` but fails here depends on this\n"
            "machine's Openplanet install or user config. Provision what it needs\n"
            "inside its own fixture dir (with an explicit --plugins-dir), or mark\n"
            "it #[ignore = \"...\"] with a named reason if it truly needs a real\n"
            "install. See tests/hermeticity_canary.rs.",
            file=sys.stderr,
        )
        return 1
    print("PASS: no test depends on this machine's HOME.")
    return 0


def run_selftest() -> int:
    """Prove the gate can fail: every canary must land on its documented side."""
    binaries = dict((name, exe) for name, exe in build_and_enumerate())
    exe = binaries.get(CANARY_TARGET)
    if exe is None:
        sys.exit(f"error: test binary `{CANARY_TARGET}` not found — was it deleted?")

    listed = subprocess.run(
        [exe, "--list", "--format", "terse"],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        text=True,
        check=True,
    )
    declared = {
        line.split(":", 1)[0] for line in listed.stdout.splitlines() if line.endswith(": test")
    }
    # Exact-set, not superset: a canary deleted or renamed out of the file must
    # break the gate loudly rather than shrink its coverage in silence.
    if declared != set(REQUIRED_CANARIES):
        missing = sorted(set(REQUIRED_CANARIES) - declared)
        extra = sorted(declared - set(REQUIRED_CANARIES))
        print(f"FAIL: `{CANARY_TARGET}` no longer declares the pinned canary set", file=sys.stderr)
        for name in missing:
            print(f"  - missing: {name} (deleted, renamed, or filtered out?)", file=sys.stderr)
        for name in extra:
            print(f"  - unpinned: {name} — add it to REQUIRED_CANARIES", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory(prefix="openplanet-lsp-hermetic-") as home:
        proc = subprocess.run(
            [exe, "--ignored", "--test-threads", "1"],
            cwd=REPO_ROOT,
            env=hermetic_env(Path(home)),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )

    observed = {
        m["name"]: m["status"].split(" ")[0]
        for m in (TEST_RESULT_LINE.match(ln) for ln in proc.stdout.splitlines())
        if m
    }

    print(f"==> {CANARY_TARGET}: {len(REQUIRED_CANARIES)} canaries under the isolated HOME\n")
    problems: list[str] = []
    seen_caught = seen_uncaught = False

    for name, want in sorted(REQUIRED_CANARIES.items()):
        status = observed.get(name)
        # The name prefix and the pinned expectation must agree, so a pinned
        # entry cannot quietly invert what its own name promises.
        prefix_want = "FAILED" if name.startswith("caught_") else "ok"
        if not name.startswith(("caught_", "uncaught_")):
            problems.append(f"{name}: name must start with `caught_` or `uncaught_`")
            continue
        if prefix_want != want:
            problems.append(f"{name}: pinned expectation {want} contradicts its `{name.split('_')[0]}_` prefix")
        seen_caught |= name.startswith("caught_")
        seen_uncaught |= name.startswith("uncaught_")
        if status is None:
            problems.append(f"{name}: did not run — is it missing #[ignore]?")
            verdict = "MISSING"
        elif status != want:
            problems.append(f"{name}: expected {want} under isolation, got {status}")
            verdict = "WRONG"
        else:
            verdict = "as documented"
        print(f"  {str(status):8}  (want {want:6})  {name:52}  {verdict}")

    if not seen_caught:
        problems.append("no `caught_*` canary exists — the gate proves nothing")
    if not seen_uncaught:
        problems.append("no `uncaught_*` canary exists — the gate's limits are undocumented")

    if problems:
        print("\nFAIL: hermetic gate self-test", file=sys.stderr)
        for problem in problems:
            print(f"  - {problem}", file=sys.stderr)
        print(
            "\nA `caught_*` canary passing under isolation means HOME was NOT\n"
            "isolated: the gate is live but blind, which is worse than no gate.\n"
            "Full canary output follows.\n",
            file=sys.stderr,
        )
        print(proc.stdout, file=sys.stderr)
        return 1

    print("\nPASS: the gate turns red on every non-hermetic canary.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--selftest",
        action="store_true",
        help="run the canaries in tests/hermeticity_canary.rs and check each "
        "lands on its documented side of the gate",
    )
    args = parser.parse_args()
    return run_selftest() if args.selftest else run_gate()


if __name__ == "__main__":
    sys.exit(main())
