# Active Goal Loop

## Primary Goal
Make `openplanet-lsp check` handle plugin dependencies and have clear help documentation.

## Acceptance Criteria
- [ ] `openplanet-lsp --help` documents the check command
- [ ] `openplanet-lsp check --help` shows all options with descriptions
- [ ] `check` command resolves dependencies from info.toml `script.dependencies`
- [ ] `check` includes dependency plugin symbols in the validation symbol table
- [ ] Running check on tm-agent, tm-mcptm, tm-aiapi completes without dependency errors

## Current Status
Iteration 1 - Assessment

## Current Plan
- Assess current state: help output, check command behavior
- Identify what's missing: help text, dependency resolution in check
- Plan fix

## Blockers / Notes
- Dependencies exist in deps.rs but check command doesn't use them
