# Plugin-fleet dogfood ledger — openplanet-lsp
# Started 2026-08-15. One block per plugin. Leave-as-found tracked per row.
# Legend: STATE = installed/running at start; VERDICT = parity class; ISSUES = GH links
#
# LSP:  openplanet-lsp check --typedb-dir tests/fixtures/typedb --plugins-dir ~/OpenplanetNext/Plugins <src>
# GAME: tm-remote-build load folder <id> -op OpenplanetNext -d <OP_DATA_DIR>  (loopback 127.0.0.1:30000)
# OP_DATA_DIR = ~/.steam/steam/steamapps/compatdata/2225070/pfx/drive_c/users/steamuser/OpenplanetNext
#
## tm-buffer-time (PILOT) — id=buffer-time
# STATE: not installed, not running. Staged by me -> unloaded+removed (restored=True).
# LSP:    0 diagnostics (exit 0)
# GAME:   12 ERR + 1 WARN (compile FAILED). Verdict: FN (game errs, LSP silent).
#   FN classes: Draw::GetHeight/GetWidth x5 (#28 family known FN);
#     'Setting_BufferFontSize' uninitialized-global x6 (NEW FN - investigate);
#     'screen' no-matching-symbol x1 (NEW FN); l-value x1 (NEW FN);
#     WARN Signed/Unsigned mismatch x1 (warning parity gap).
#   Note: game dedups MLHook export_dep ("already a dependency") - matches LSP BFS.
# ISSUES: <pending triage - see below>
# LOOP VALIDATED: snapshot->lsp->stage->load->oplog->compare->unload->remove all green.
# Driver: scripts/fleet_dogfood.py (state in docs/plugin-fleet-ledger.json).
#
# DRIVER FIX (2026-08-16): first sweep version UNLOADED already-running plugins
# (Editor, tm-control-mcp) — leave-as-found violation. Fixed: reload already-installed
# plugins to capture compile output but NEVER unload them; full stage->load->unload->
# remove cycle ONLY for fresh-staged (previously-absent) plugins. Orphaned
# customize-cp-counter folder removed after the killed first sweep.
# NOTE: tm-control-mcp + E++ are another agent's — do not touch.
#
# SETTINGS-INIT ROOT CAUSE (tm-buffer-time): Setting_BufferFontSize is a [Setting]
# global initialized by `60 * Draw::GetHeight() / 1440` (KoBufferDisplay.as:379).
# Draw::GetHeight() is a runtime UI fn with no valid context at global-init time,
# so the global fails to construct => every later read reports "Use of uninitialized
# global variable" (6x cascade, NOT 6 independent bugs). Same mechanism: `auto screen
# = vec2(Draw::GetWidth(),...)` — Draw::* fails => auto has no type => later `screen`
# uses are "No matching symbol". TRUE ROOTS are few: Draw::* (known #28) + l-value +
# signed/unsigned. Triage must collapse cascade echoes to their root.
#
# ===== FLEET SWEEP COMPLETE (2026-08-16) =====
# 129/142 fully compared. Verdict distribution: 65 CLEAN, 27 BOTH, 25 FN, 13 FP.
# 13 plugins GAME-SKIP (RemoteBuild died near end of sweep; port accepts but
#   doesn't respond — game-state issue, not fixable from here). Re-run pending:
#   tm-cotd-lb-toggle tm-customize-cp-counter tm-tmx-together tm-too-many-ghosts
#   tm-tweaker-reborn tm-unbeaten-ats tm-unintrusive-checkpoint-timer tm-upload-all-tmx
#   tm-upload-map-to-nadeo tm-video-player tm-view-profile-demo tm-vip-everyone tm-warp-to-waypoint
# Issues filed (sweep-first, fixes deferred to prioritized batch):
#   #32 FP const-property getter 'has no member' (tm-agent; naive repro CLEAN — needs bisect)
#   #33 FP dep-id unresolved when = display-name not module/folder (BetterRoomManager; bosslike+simple-room-admin)
#   #34 FP overload set not merged across files/exports (UpdateFrom 2-arg; tm-mlfeed-race-data)
#   #35 Gap typedb missing TM2/MP4 types CTrackManiaRaceNew/ScriptPlayer (+ dead #if MP4 checked?)
#   #36 FP undefined-ident game accepts (WheelsStartOffset, DS_AP_NAME — no in-source decl)
#   #37 FN/warning batch: signed-unsigned(31) float-trunc(22) unreachable(9) sign-change(7)
#         shadow(N) read-before-init(N) dup-fn(18) overload-sig(N); Draw::*(64/50)=#28, Text::Join=#18/#28
# Full dedup gap table: /tmp/fleet-sweep-full.log (regenerate via `sweep` once RB healthy).
# State: docs/plugin-fleet-ledger.json. Leave-as-found verified (folders + OP-log load/unload pairs).
