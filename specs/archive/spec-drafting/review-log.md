# Review Log

## micro-review v0.1 — Section 1 (Problem and goals)

**Type:** micro-review
**Scope:** Section 1
**Findings:** Problem statement accurate to source evidence.
**Fixes:** None needed.

## Checkpoint A — Foundation (v0.2)

**Type:** checkpoint review
**Scope:** Sections 1-3
**Findings:** All foundation decisions confirmed. No contradictions.
**Fixes:** Section 1 problem wording updated.

## micro-review v0.3 — Sections 4-6 (Behavior)

**Type:** micro-review
**Scope:** Sections 4, 5, 6
**Findings:** Config mechanism was UNRESOLVED; now resolved (D-008, D-009). Lambda support unclear (deferred as D-06). `[Persistent]` attribute observed in plugins but not in official docs.
**Fixes:** None blocking.

## Checkpoint D — Full Consolidation (v0.6)

**Type:** full consolidation review
**Scope:** All sections (1-16)
**Findings:**
1. **Terminology normalization:** "type JSON database", "type DB", "type info JSON files" used inconsistently. Acceptable since meaning is clear from context; no fix needed.
2. **Decision notes summary was incomplete:** Missing D-008 and D-009. FIXED.
3. **Default defines incomplete:** Missing MANIA32, WINDOWS_WINE, LINUX, SERVER, LOGS, HAS_DEV, DEVELOPER for the all-permissive set. FIXED.
4. **A-09 resolved:** Configuration mechanism settled by D-008/D-009. Marked as resolved. FIXED.
5. **Acceptance criteria vs UX targets:** AC-08 (2s startup) is more generous than 10.1 (1s target). This is correct — targets are aspirational, acceptance is the bar.
6. **No stale unresolved items remain.** All UNRESOLVED markers from earlier drafts are settled.
7. **Cross-section consistency verified:** FR requirements match acceptance criteria; data structures match functional needs; interfaces reference correct sections.
8. **Provenance labels internally consistent.** No blended labels found.
9. **Lambda/inline functions:** Mentioned in 5.1 with "(if supported)" caveat; tracked as D-06. Correct handling.
**Fixes applied:** 3 edits (decision summary, default defines, A-09 status).
**Open concerns:** None blocking. Draft meets readiness gate.
