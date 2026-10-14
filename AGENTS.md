
- When fixing bugs, always use TDD principles + red/green tests.
- When adding new test cases, try to generalize the case and create other variants of it. For example, if we needed to test a `const T &in` function argument, we shoudl also have tests for `&out` and non-const and `T@` and `T@ &in` etc.
- A vscode compatible extension is in ../lsp-openplanet-ext/
- Heavy builds/tests should be offloaded to a build box: `just rtest` / `rcheck`
  / `rclippy` / `rrun` (see `scripts/remote-build.py`, `just rdoctor` first).
  Defaults to x-alpha. Works from a lane worktree; each worktree gets its own
  remote tree and target dir. Prefer this over local `just test` whenever Max is
  at the keyboard — a local full suite saturates his 16 cores.
- Run `cargo fmt --all` in your worktree before you report work as ready. It is
  cheap (no compilation) and it keeps a lane's diff reviewable. It is
  deliberately NOT a gate: a gate would fail every lane that has a formatting
  nit mid-work, which is most of them most of the time.
  The coordinator runs `cargo fmt --all` before merging a branch that skipped
  it, and commits that repair separately from the lane's work — so an
  unformatted branch is a tidy-up, never a rejection. (B159, ruling by Max
  2026-09-01.)
- Develop every Openplanet spike plugin in its own subfolder under the
  Openplanet plugin workspace. A spike folder must contain its own `info.toml`
  and `src/` before using any plugin build or staging script.
- Treat Openplanet `build.sh` scripts as destructive staging commands: inspect
  the script first, run it from the plugin's own root, and pass an explicit
  `PLUGINS_DIR` or `OPENPLANET_DIR`. Verify that the destination is the intended
  plugin subfolder before running it because these scripts may use
  `rsync --delete`. Never run a plugin `build.sh` from this repository root or
  with this repository as the staging destination.
