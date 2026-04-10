# Interview Log

## Round 0 — Preflight (source survey, no user questions)

**Slice:** Preflight / all sections
**Sources read:** ~/scrape/openplanet (858 files), ~/scrape/angelcode (319 files), ~/src/openplanet (154 plugins), ~/src/openplanet/vscode-openplanet-angelscript (existing LSP audit)
**Draft changes:** Created specs/draft-v1.md skeleton (v0.1)

---

## Round 1 — Foundation + Scope

**Slice:** Sections 1-3
**Questions:** Implementation language? Fork vs. fresh? Editor clients? Game target? Web Services API?
**User answers:**
1. Rust — fast, accurate, low memory/CPU
2. Fresh build — existing LSP is "algorithmically bad"
3. Generic stdio LSP
4. TMNEXT primary, JSON format same across games, preprocessor #if is the hard part
5. Web Services: No — no native OpenPlanet support
**Additional:** info.toml must be parsed with diagnostics. Fixture tests from real plugins, all errors accounted for.
**Draft changes:** Sections 1-3 reviewed. v0.2.

---

## Round 2 — Behavior + Configuration

**Slice:** Sections 4-6 + configuration
**Questions:** How to discover type JSON / plugins dir? Default preprocessor defines?
**User answers:**
1. Layered: auto-detect → config file → init params
2. Default defines: all-permissive (all game targets, platform, SIG_DEVELOPER). User listed: TMNEXT, MP4, TURBO, UNITED, WINDOWS, LINUX, SIG_DEVELOPER, SIG_REGULAR.
**Draft changes:** Sections 4-6 drafted, config resolved. v0.3.

---

## Round 3 — Systems + Closure

**Slice:** Sections 7-16
**Questions:** None needed — sufficient evidence from source material.
**Draft changes:** All remaining sections drafted. v0.4, v0.5.

---

## Checkpoint D — Full Consolidation

**Scope:** All sections
**Changes:** Decision notes completed, default defines expanded to full permissive set, A-09 resolved, terminology reviewed. v0.6.
**Result:** All sections reviewed. No blocking issues. Ready for user review.
