
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
  it, and commits that repair separately from the lane's work — so an unformatted
  branch is a tidy-up, never a rejection. (B159, ruling by Max 2026-09-01.)
