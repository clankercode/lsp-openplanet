
- When fixing bugs, always use TDD principles + red/green tests.
- When adding new test cases, try to generalize the case and create other variants of it. For example, if we needed to test a `const T &in` function argument, we shoudl also have tests for `&out` and non-const and `T@` and `T@ &in` etc.
- A vscode compatible extension is in ../lsp-openplanet-ext/
- Heavy builds/tests should be offloaded to a build box: `just rtest` / `rcheck`
  / `rclippy` / `rrun` (see `scripts/remote-build.py`, `just rdoctor` first).
  Defaults to x-alpha. Works from a lane worktree; each worktree gets its own
  remote tree and target dir. Prefer this over local `just test` whenever Max is
  at the keyboard — a local full suite saturates his 16 cores.
