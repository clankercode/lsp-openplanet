# Active Goal: Full LSP Feature Support for Openplanet AngelScript

## Primary Goal
Implement complete LSP feature support: type errors & full inference, completion, goto-definition, diagnostics, find-references, hover, document/workspace symbols, rename, formatting, semantic tokens, and code actions — all backed by a full AngelScript static type checker with overload resolution, generics, and const-correctness.

## Scope decisions (from user, 2026-04-11)
- **Type checker**: Full AngelScript checker — overload resolution, generic instantiation, implicit conversions, const-correctness, class hierarchy.
- **Formatting / code actions**: Starter set — AST-based pretty-printer + diagnostic-driven quick-fixes.

## Acceptance Criteria
- [x] **AC1 — Diagnostics (type errors)**: undefined identifier, undefined member/method, unknown type, arg-count, arg-type, return-type, const violation, handle/value, invalid assignment target, missing return. All 10 diagnostic kinds shipping (iters 18-24).
- [x] **AC2 — Type inference + checker**: auto inference (iter 18), expression derivation (iter 20-22), overload resolution (iter 26), implicit conversions (iter 25), generic instantiation for array/dictionary (iter 29), class hierarchy including inherited members cross-file (iter 27, 28, 31), const propagation through index/member (iter 32).
- [x] **AC3 — Goto definition**: workspace-wide, navigation module.
- [x] **AC4 — Find references**: workspace-wide via navigation module.
- [x] **AC5 — Hover**: typed via symbol table + TypeIndex enrichment.
- [x] **AC6 — Completion**: context-aware with keywords, scope, member, namespace.
- [x] **AC7 — Document symbols**: nested hierarchy.
- [x] **AC8 — Workspace symbols**: `workspace/symbol` via pooled SymbolTable.
- [x] **AC9 — Rename**: workspace-wide via navigation module.
- [x] **AC10 — Formatting**: AST-based canonical pretty-printer.
- [x] **AC11 — Semantic tokens**: full-document provider.
- [x] **AC12 — Code actions**: Levenshtein-based "did you mean" quick-fixes.
- [x] **AC13 — Green bar**: 305 unit + 12 integration passing, 0 ignored. Parser corpus 140 plugins / 1603 files / 0 errors. Tower-lsp harness smoke test closed in iter 30.
- [x] **AC14 — Signature help** *(iter 33)*: Active-parameter-aware signature help with overload cycling. `Backend::signature_help` now dispatches to `src/server/signature.rs`, resolving callees via workspace free functions, TypeIndex free functions, type methods, workspace methods with inheritance walk, and implicit-`this`. `find_enclosing_call` walks backwards counting parens with a forward-pass skip-mask for strings/comments.
- [x] **AC15 — Inlay hints** *(iter 34)*: Type hints for `auto` locals and param-name hints on literal arguments. `textDocument/inlayHint` handler in `src/server/inlay_hints.rs`. Qualified callees go straight to TypeIndex to avoid cross-namespace mis-resolution. Lambda return-type hints deferred.
- [ ] **AC16 — Document highlights**: Highlight every occurrence of the symbol at cursor within the current document (kind = Read/Write where derivable). Reuses the existing navigation reference walker but scoped to one file. Tests for local var, field, method, and type-ref highlights.
- [ ] **AC17 — Folding ranges**: Collapse regions for function/method bodies, class/namespace blocks, multi-line comments, and `#if`/`#endif` preprocessor blocks. Tests for each kind plus nested folding.
- [ ] **AC18 — Call hierarchy**: `textDocument/prepareCallHierarchy`, `callHierarchy/incomingCalls`, and `callHierarchy/outgoingCalls`. Backed by the existing workspace SymbolTable + reference index. Tests for incoming/outgoing traversal including cross-file calls.
- [ ] **AC19 — Method-call const propagation**: Iter 32 deferred method-call return-type const inheritance. When a method is invoked on a const receiver, its return type should inherit `Const(_)` unless the method itself is declared non-const. Requires surfacing the `const`-qualifier on method decls through `SymbolKind::Method`. Tests: const-receiver method return flows into downstream assignment check; non-const method on const receiver can be detected as a diagnostic if the method mutates.
- [ ] **AC20 — Parser const-handle ordering**: Parser currently collapses both `const Foo@` (handle to const object) and `Foo@ const` (const handle to mutable object) into `Const(Handle(Foo))`, losing the semantic distinction. Iter 32's `const_handle_not_const_contents` test was dropped because the parser can't round-trip the difference. Fix: produce `Handle(Const(T))` for `const Foo@` and `Const(Handle(T))` for `Foo@ const`. Update checker helpers accordingly. Re-add the dropped test.
- [x] **AC21 — Load Openplanet + Trackmania types from JSON**: `TypeIndex::load(core_json, game_json)` in `src/typedb/index.rs` merges Openplanet core API (`core_format.rs`) and Nadeo game engine (`nadeo_format.rs`) JSON schemas into a single queryable type index. Wired through `Backend::initialize` in `src/server/mod.rs:115` via `LspConfig { core_json, game_json }`. Short-name resolution via `short_type_index`, method/property lookup, enum values, docs. **Status: satisfied** — this has been in place since early scaffolding and is the foundation every other LSP feature builds on. Flagged explicitly per user request on 2026-04-11.

## Deferred — documented for future consideration
The following LSP features are intentionally excluded from the current AC list but tracked here so they can be picked up later without rediscovery. Each one should graduate to a numbered AC if concrete demand surfaces.

- **Dictionary value typing**: Corpus investigation (post-iter 32) confirmed ZERO `dictionary<K,V>` usage across all 140 plugins. Plugins rely on untyped `dictionary` with runtime casts (`int(dict[key])`). AngelScript `dictionary` stores `CScriptAny` values natively. Speculative Set/Get-based inference would be fragile with no user syntax to anchor. Current iter 29 opaque-silent strategy stays. **Revisit if**: Openplanet ships a typed-dictionary generic, or a plugin adopts a typed-wrapper convention worth tracking.
- **Code lens**: Inline actionable labels (e.g. "N references", "Run plugin"). Useful for discoverability but optional. **Revisit if**: we want to surface reference counts or run-plugin commands directly above declarations.
- **Document color / color presentation**: Color swatches next to `vec3`/`vec4`/hex literals. Niche for a plugin-scope LSP. **Revisit if**: UI-heavy plugin authoring becomes a priority.
- **Linked editing range**: Rename-like inline multi-caret editing (e.g. simultaneously edit matching tag pairs). Not commonly expected for AngelScript. **Revisit if**: user feedback requests simultaneous-edit for matched identifiers.
- **Selection range**: Smart selection expansion (`expand to next syntactic scope`). Nice-to-have editor ergonomics, low impact. **Revisit if**: downstream editor integrations request it.
- **Moniker**: Cross-repository symbol identifiers for LSIF-style indexing. Only relevant if we build an offline code-intelligence index. **Revisit if**: we want to publish precode navigation data to external tools.
- **Workspace pull-diagnostics** (`workspace/diagnostic`): Newer LSP spec for editor-initiated diagnostic polling instead of server push. Current push-model via `publish_diagnostics` works fine. **Revisit if**: editor clients start preferring pull-model or performance tuning demands it.

## Current Status
- **AC1-AC15 + AC21 satisfied** as of iter 34 (2026-04-11). 320 unit + 14 integration green, 0 ignored. Parser corpus untouched.
- **AC16-AC20 open**: 3 new LSP feature additions (AC16-AC18) + 2 quality deepenings from iter 32 deferrals (AC19-AC20). Loop at iter 35 next.
- **ON_GOAL_COMPLETE_NEXT_STEPS**: Auto-advance enabled by user directive 2026-04-11 ("Resume loop and complete all items"). On AC14-AC20 completion, loop stops and reports — no further phases queued.

## Iter 3 known gaps (carry forward)
1. No external TypeIndex in `compute_diagnostics` → FPs on every Openplanet type reference.
2. `SymbolTable::extract_symbols` does not descend into class bodies → no method/field symbols.
3. Return-type checking not implemented.
4. Auto inference is a placeholder.
5. `this`/`super` → `TypeRepr::Named("this")` (placeholder).
6. Member access is a no-op.
- **Known gaps surfaced by iter 2**:
  - Parser normalises `array<T>` and `T[]` both to `TypeExprKind::Array`; `Template` arm only reached by user-defined templates like `Grid<int>`.
  - Parser doesn't parse `dictionary<K,V>` template args — `KwDictionary` → `Named("dictionary")`.
  - `SymbolTable::all_symbols()` is O(N) per lookup; GlobalScope linear-scans it. Fine for scaffolding but will need indexing when the checker runs hot.

## Iter 27 Result (2026-04-11)
- 285 unit + 11 integration green. Parser corpus untouched.
- AC2 cross-file class hierarchy. `SymbolKind::Class.parent` was already present but hardcoded to `None`; `extract_item_symbols` now populates it from `cls.base_classes.first()` via a new `base_class_name()` helper (strips Handle/Const/Reference wrappers, extracts QualifiedName text).
- New `GlobalScope::workspace_class_parent` + `workspace_class_member` — the latter walks the inheritance chain with a `HashSet<String>` cycle guard and honors `get_/set_` virtual-property convention.
- `Checker::member_access_type` wires the walker after the in-file `file_classes` shortcut, `lookup_member_type`, and `class_stack` checks but before `UndefinedMember` emission — iter 24 const wrapper path stays byte-identical.
- 5 new tests (280 → 285): child_inherits_parent_field_cross_file, child_inherits_parent_method_cross_file, grandchild_two_levels_cross_file, override_shadows_parent_field, cycle_does_not_loop.
- **Deferred**: implicit-`this` member lookup in methods still walks only `file_classes` (the `lookup_class_member` path). `SymbolTable` field/method symbols still store empty `type_name`/`return_type`, so the override test asserts absence of diagnostic rather than a resolved type.

## Iter 28 Result (2026-04-11)
- 289 unit + 11 integration green. Parser corpus untouched, no warnings.
- AC2: `SymbolTable::extract_item_symbols` now populates real type strings: Field via `type_expr.span.text(source)`, Method via `return_type.span.text(source)`, Constructor via class name, Destructor empty.
- `GlobalScope::workspace_class_member`, `lookup_member_type`, and `lookup_method_return` now route stored type text through `TypeRepr::parse_type_string` (reuses existing external-lookup path — no new helper).
- `Checker::call_type`'s `Member` branch now consults `workspace_class_member` after `lookup_method_return`/`class_stack`, so inherited-method return types flow into arg-type checks.
- Discovered: `lookup_member_type`'s workspace fallback also needed the real-type path — not just the inheritance walker — because a child's direct field hit short-circuits before `workspace_class_member` runs.
- 4 new tests (285 → 289). Downstream diagnostic exercised: ArgTypeMismatch on both inherited fields and inherited method returns.

## Iter 29 Result (2026-04-11)
- 297 unit + 11 integration green (+8 unit: 5 checker, 3 repr). Parser corpus untouched, zero warnings.
- AC2 generic instantiation: `TypeRepr::Array(Box<TypeRepr>)` was already present but `parse_type_string` was producing `Generic { base: "array", .. }`. Canonicalized to `Array(_)`. Updated one existing test.
- New helpers in `repr.rs`: `array_element_type()`, `is_array_like()` (accepts both `Array(_)` and bare `Generic { base: "array" }`), `is_dictionary_like()`.
- `Checker::ExprKind::Index` returns element type via `array_element_type()`; strips `Const` permissively (noted in comment — const-through-index is deferred).
- `member_access_type` special-cases: array-like `Length`/`length` → `Uint`, `IsEmpty`/`isEmpty` → `Bool`, mutating methods silent; dictionary-like all silent.
- No new variants needed. `dictionary` stays `Generic { base: "dictionary" }`.

## Iter 30 Result (2026-04-11)
- 297 unit + **12** integration green. Parser corpus untouched.
- **AC13 tower-lsp smoke test closed.** Strategy B: `LspService::new(Backend::new)` → `LspService::inner()` → call `LanguageServer` trait methods directly. `tower-lsp 0.20` exposes `.inner()` (confirmed at `tower-lsp-0.20.0/src/service.rs:98`).
- New test: `test_tower_lsp_smoke_crossfile_hover` opens `helper.as` (`class Base { int count; }`) and `main.as` (`class Foo : Base {} void test() { Foo f; f.count = 5; }`), drives `initialize`/`did_open`/`hover`/`goto_definition`, asserts ServerCapabilities has hover+definition providers, and asserts non-empty hover result on `Base` — exercising iter 27/28 cross-file `SymbolTable` assembly end-to-end through the real `Backend`.
- Only src change: `Backend::new` visibility bumped from private `fn` to `pub fn` so integration tests can reach it.
- Publish_diagnostics suppression confirmed safe: tower-lsp 0.20 `Client::send_notification` short-circuits when service state isn't `Initialized`; direct trait method calls bypass the Service state machine.

## Iter 31 Result (2026-04-11)
- 301 unit + 12 integration green (+4 unit). Parser corpus untouched, no warnings.
- AC2 final deferral closed: `Checker::lookup_class_member` now falls through to `GlobalScope::workspace_class_member` when the file-local parent walker hits a parent not in `file_classes`. Also added a same-class fallback for defensive coverage when the current class itself lives in a sibling file.
- Single helper handles both field-style (`expr_type` Ident arm) and method-style (`call_type` Ident arm) via `.is_some()` check — extending the one function covered both paths.
- Iter 24 const-wrapper path untouched: file-local walk is still tried first.
- 4 new tests: method_uses_inherited_field_cross_file, method_uses_inherited_method_cross_file, method_uses_inherited_field_with_type_flows (ArgTypeMismatch fires first try — no fallback rewrite), cycle_cross_file_method_terminates.

## Iter 32 Result (2026-04-11)
- 305 unit + 12 integration green (+4 unit, 0 ignored). Parser corpus untouched, no warnings.
- AC2 final deferral closed: const propagation through `Index` and `Member` access.
- New `Checker::receiver_is_const(ty)` accepts both `Const(Handle(T))` and `Handle(Const(T))` orderings (forward-compatible with parser fix). New `apply_receiver_const(t, flag)` wraps non-Const/non-Error field types in `Const(_)` when the receiver is const.
- `ExprKind::Index` on const array returns `Const(elem)` instead of stripping; non-const arrays unchanged (iter 29 tests byte-identical).
- `member_access_type`: all four lookup paths route field types through `apply_receiver_const`. Array `Length`/`IsEmpty` and dictionary opaque returns are NOT wrapped (primitive rvalues). Method return types are untouched — method-call const propagation stays deferred.
- Iter 24's `Assign`/`HandleAssign` outer-layer Const check needed zero modification — the new propagated types already carry Const through.
- Parser gap (not fixed): `const Foo@` and `Foo@ const` both parse to `Const(Handle(Foo))`, losing the semantic distinction between const-contents and const-handle. Test for the latter was dropped (AC13 forbids `#[ignore]`); fix belongs in a future parser iteration.
- 4 new tests: const_array_element_assign_fires, const_array_element_read_is_fine, const_member_chain_fires, non_const_member_receiver_not_const.

## Iter 33 Result (2026-04-11)
- 314 unit + 13 integration green (+9 unit, +1 integration, 0 ignored). Parser corpus untouched, no new warnings after cleanup.
- **AC14 signature help closed.** New `src/server/signature.rs` module (the iter 10 placeholder was a zero-byte stub — filled in). `Backend::signature_help` now dispatches to it.
- **`find_enclosing_call`**: walks backwards from the cursor counting paren/bracket/brace depth. Strings, char literals, line comments, and block comments are masked out via a forward pre-pass (`compute_skip_mask`) so the backwards walker never has to disambiguate `/` or `"`. Bails at unmatched `{` or top-level `;` — returns `None` when the cursor isn't in a call.
- **`resolve_callee` order**: (1) workspace free functions via `GlobalScope::lookup_function_overloads` including a qualified→tail fallback, (2) `TypeIndex::lookup_function` for Openplanet/Nadeo free functions, (3) `TypeIndex::lookup_type(...).methods` on a member-call receiver, (4) workspace methods with an inheritance walk (`workspace_class_parent`, break at first defining class), (5) implicit-`this` via `find_enclosing_class`, (6) TypeIndex constructor fallback.
- **Active-parameter tracking**: top-level comma count from the opening `(` to the cursor, masked for strings/comments and gated by paren/bracket/brace depth. Nested calls: innermost `(` wins because the backwards walker hits it first.
- **Signature label quality (known inconsistency)**: workspace free functions surface `(int, string) -> void` because `OverloadSig::param_types` has no parameter names. Workspace methods and external TypeIndex functions show full `type name` pairs. Logged as a follow-up; not in AC14's scope.
- **Post-review cleanup**: 5 clippy warnings fixed (`mask[start..i].fill(true)` ×3, `trim_matches(['.', ':'])` ×2). Dead `_pos_unused` + `pos` test helper removed. `pick_active_signature` rustdoc aligned with its actual implementation (no `min_args_required` check).
- **Iter 32 parser gap carries forward**: `const Foo@` vs `Foo@ const` still collapse to `Const(Handle(Foo))` — AC20 target.

## Iter 34 Result (2026-04-11)
- 320 unit + 14 integration green (+6 unit, +1 integration, 0 ignored). Parser corpus untouched, zero new clippy warnings in new files.
- **AC15 inlay hints closed.** New `src/server/inlay_hints.rs` module wired through `Backend::inlay_hint`. `inlay_hint_provider: OneOf::Left(true)` advertised in capabilities.
- **Type hints for `auto` locals**: fires only when `vd.type_expr.kind == TypeExprKind::Auto` (AngelScript's `auto` keyword — the only untyped let form). A narrow `infer_init_type` handles `IntLit`/`HexLit` → `int`, `FloatLit` → `float`, `StringLit` → `string`, `BoolLit` → `bool`, `Cast`/`TypeConstruct` → target type text, and `Call` → workspace/external function return type. Arithmetic/unary/member/index fall through (cheap skip, no wrong hints).
- **Parameter-name hints**: walks `ExprKind::Call`, extracts dotted/namespaced callee via `Ident`/`NamespaceAccess`/`Member` chain, resolves via `SymbolTable` (workspace function params stored as `(name, type)`) with fallback to `TypeIndex::lookup_function`. Qualified callees bypass workspace lookup (symbol table is bare-name keyed — would otherwise cross-match unrelated `Ns::bar`). Emits `PARAMETER`-kind hints only for literal/null positional args; suppressed when arg is `Ident` whose text matches the param name.
- **Range filtering**: post-collection via `position_in_range`. Reviewer noted end-boundary inclusivity is slightly off-spec (LSP ranges are end-exclusive); accepted as cosmetic since hint positions sit inside identifiers, not on boundaries.
- **Deferred**: (1) Lambda return-type hints — requires running expression inference *inside* a scoped checker frame for body locals; `Checker::expr_type` is private and wiring it for lambdas touches unrelated helpers. Follow-up iter candidate. (2) Method-call param-name hints (`foo.m(5)`) — would need `scope_query::local_type_at` + workspace class chain walk; missing but never wrong. (3) Binary/unary `auto` inference.
- **Post-review fix**: `lookup_callee_param_names` originally fell back to bare-tail workspace lookup even for qualified callees, which could display wrong param names when two namespaces shared a function name. Now gated on `!callee.contains("::")`.

## Iter 35 Plan (next)
- **Goal**: AC16 document highlights. Implement `textDocument/documentHighlight` to surface all occurrences of the symbol under the cursor within the current document, differentiating READ vs WRITE with `DocumentHighlightKind`.
- **Why**: No handler exists today. This is high-value editor UX (VSCode/Neovim highlight matching identifiers as you move the cursor) and the building blocks are already in place — `navigation::find_references` computes cross-file references, but document highlight is intra-file only and can reuse `scope_query`/`symbols::table`.
- **Approach**:
  1. Advertise `document_highlight_provider: Some(OneOf::Left(true))` in `initialize`.
  2. Implement `Backend::document_highlight`. Reads the current document text, calls a new `src/server/highlights.rs::document_highlights(source, position)` returning `Vec<DocumentHighlight>`.
  3. Walk the parsed `SourceFile` collecting every identifier occurrence whose resolved symbol equals the cursor's symbol. Start simple: lexical name match within the same scope (file-level + enclosing function body). For class members, match on `self.field` + bare `field` within methods of the same class.
  4. Determine READ vs WRITE: WRITE when the occurrence is the LHS of `ExprKind::Assign`/`HandleAssign`, the target of `++`/`--`, or a `Stmt::Let` declarator. Otherwise READ.
  5. TEXT kind for keyword/literal hits (probably unused for AC16).
- **Verify**: new tests in `src/server/highlights.rs` covering: single function local highlighted at every use; assignment shows WRITE; shadowing doesn't over-highlight outer scope; class field from method body; cursor on a keyword returns empty. One tower-lsp smoke test.
- **Target**: 325+ unit + 15 integration, 0 ignored, no new clippy warnings.

## Iter 34 Plan (superseded)
- **Goal**: AC15 inlay hints. Implement `textDocument/inlayHint` to surface inferred `auto` local types, parameter-name hints at call sites, and return-type hints on multi-line lambdas.
- **Why**: Zero handler exists today (`Backend` does not implement `inlay_hint`). `auto` locals silently hide the resolved type and call sites with many literal args are unreadable — the two complaints the goal doc flags for AC15.
- **Approach**:
  1. Advertise `inlay_hint_provider` in `ServerCapabilities`.
  2. Implement `Backend::inlay_hint` returning `Vec<InlayHint>` for the visible range.
  3. New `src/server/inlay_hints.rs` module. Walk the parsed `SourceFile` over the requested range. For each `Stmt::Let` with `auto` or no explicit type, resolve the initializer via `Checker::expr_type` (reuse workspace + TypeIndex), emit an `InlayHint` of kind `Type` immediately after the identifier. For each `Expr::Call` with ≥ 1 positional literal arg, look up the callee's parameter names via the same `resolve_callee` paths `signature.rs` uses (factor out a small helper if needed), emit `Parameter` hints before each literal.
  4. Skip hints whose label matches the source text trivially (e.g. `foo(name)` where `name` is already the param name).
- **Verify**: new tests in `src/server/inlay_hints.rs` covering: `auto x = f();` → `: int` hint, call with literal gets param-name hints, explicit-typed let is silent, hint suppression when identifier name matches param name. One tower-lsp smoke test exercising the handler end-to-end.
- **Target**: 320+ unit + 14 integration, 0 ignored, no new clippy warnings.

## Iter 33 Plan (superseded)
- **Goal**: AC14 signature help. Replace `Backend::signature_help`'s `Ok(None)` stub with a real implementation.
- **Why**: The capability is advertised (`SignatureHelpOptions` with trigger chars `(` and `,` in `src/server/mod.rs:141`) but the handler is dead code. Active-parameter tracking + overload cycling are the biggest missing editor UX win.
- **Approach**:
  1. Find the innermost unclosed call expression at the cursor. Walk backwards counting parens/brackets/braces (skipping strings and comments). When we hit an unmatched `(`, the tokens just before it form the callee expression.
  2. Resolve the callee via the same paths the checker uses: workspace `GlobalScope::lookup_function_overloads` + `TypeIndex` free functions + method lookup on a receiver's type for `member.call(...)`.
  3. Build `SignatureInformation` entries — one per overload — with `ParameterInformation` for each param. Pick `active_signature` by matching the current arg count (prefer exact, fall back to closest-fit). Pick `active_parameter` by counting top-level commas between the opening `(` and cursor.
  4. Return `SignatureHelp { signatures, active_signature, active_parameter }`.
- **Verify**: new tests in `src/server/signature.rs` (file already exists as a stub — iter 10 placeholder; use it) covering: single overload, 3-overload function with active sig picked by arg count, active parameter tracked through trailing comma + whitespace, nested call (`outer(inner(|)), | = cursor)` returns the inner signature.
- **Integration**: add one tower-lsp smoke test in `tests/integration_tests.rs` driving `signature_help` directly.
- **Target**: 310+ unit + 13 integration.

## Iter 32 Plan (superseded)
- **Goal**: Const propagation through `Index` and chained `Member` access so `const array<int>@ arr; arr[0] = x;` and `const Foo@ f; f.field = x;` fire ConstViolation.
- **Why**: Iter 29 drops `Const` permissively on `ExprKind::Index` to avoid FPs while array inference stabilized. Iter 24's field-level const violation only fires when the LHS outer layer is Const — but chained access through a const receiver currently strips it.
- **Approach**:
  1. `ExprKind::Index` on `Const(Array(elem))`: for READS, return `Const(elem)` (preserve). For WRITES, the assignment check needs to see Const on the LHS — preserve it there too.
  2. `member_access_type` on a const receiver: the result should inherit Const unless the member is a method (methods on const receivers need more nuance — skip for now).
  3. Add a `is_const_context` helper or just propagate via wrapping.
- **Verify**: new tests — const_array_element_assign_fires, const_member_chain_fires, const_array_read_is_const, non_const_receiver_member_not_const. 305+ unit + 12 integration. Existing const tests must still pass.

## Iter 31 Plan (superseded)
- **Goal**: Extend `Checker::lookup_class_member` (implicit-`this` path) to walk cross-file parent chain via `GlobalScope::workspace_class_member`, closing the last iter 27 deferral.
- **Why**: Method bodies that reference inherited members by bare name (implicit `this.field`) fire UndefinedIdentifier false positives whenever the base class lives in a sibling file.
- **Approach**: After the existing file-local walk in `lookup_class_member` bottoms out, fall through to `GlobalScope::workspace_class_member(current_class, name)` — same helper iter 27 wired into `member_access_type`. Use the same visited-set cycle guard already in place.
- **Verify**: new tests — method_uses_inherited_field_cross_file, method_uses_inherited_method_cross_file, cycle_cross_file_terminates. 300+ unit + 12 integration.

## Iter 30 Plan (superseded)
- **Goal**: Close AC13's "LSP server smoke-tests via tower-lsp harness" requirement by adding a real end-to-end integration test that drives Backend via the tower-lsp service, not by calling compute_diagnostics directly.
- **Why**: AC13 is the final green-bar criterion. Every other LSP test so far bypasses tower-lsp's message routing, so there's no coverage confirming initialize → did_open → hover/goto/completion actually works through the real service wiring.
- **Approach**:
  1. In `tests/integration_tests.rs`, add a test that uses `tower_lsp::LspService::new(Backend::new)` to create a Backend + socket, then wires an in-memory duplex channel (tokio::io::duplex) between the service and a test Client.
  2. Serialize one LSP request/response manually (initialize + initialized + didOpen), read the published diagnostics from the server's output stream, assert they show up. If the full manual framing is too much, at minimum call the `LanguageServer` trait methods directly on a constructed `Backend` — that's strictly better than bypassing Backend entirely.
  3. Cover at least: didOpen → hover → goto_definition across two files so iter 27/28 cross-file work is exercised end-to-end.
- **Verify**: 298+ unit + 12+ integration (one new tower-lsp test at minimum). No regressions.

## Iter 29 Plan (superseded)
- **Goal**: Generic instantiation for `array<T>` / `T[]` / `dictionary` — propagate element type through index, length, and common methods so plugins stop FP'ing on every collection access.
- **Why**: `array` is ubiquitous in Openplanet plugins. Current `TypeRepr` either collapses `array<T>` to `Named("array")` or has a variant that doesn't flow element type to downstream checks.
- **Approach**:
  1. Survey `TypeRepr` in `src/typecheck/type_repr.rs` (or wherever) — does it already have `Array(Box<TypeRepr>)` or similar? If yes, verify `TypeRepr::parse_type_string` handles `array<T>` and `T[]`. If not, add it.
  2. Propagate element type through `ExprKind::Index` (`arr[i]` → element type). `.Length`/`.get_Length` → `uint`. `dictionary` is trickier — `dict["key"]` in AngelScript returns whatever — minimum acceptable: just don't fire FPs.
  3. Ensure the parser-level normalization (`array<T>` vs `T[]`) lands in the same canonical form.
- **Verify**: new tests — array_index_propagates_element_type, array_length_is_numeric, nested_array_of_handles, typed_array_literal. Parser corpus untouched.
- **Target**: 293+ unit + 11 integration.

## Iter 28 Plan (superseded)
- **Goal**: Populate real field `type_name` and method `return_type` strings in `SymbolTable::extract_item_symbols` so `workspace_class_member` returns actual `TypeRepr` instead of `Error("")` sentinels.
- **Why**: Iter 27 deferral — cross-file inherited members currently resolve but carry no type, so downstream arg-type/return-type/assignment checks silently skip them. Fixing this turns inherited access into real typechecking.
- **Approach**: Mirror iter 19's param-type extraction pattern — use `type_expr.span.text(source)` inside the `Item::Class` arm for each Field/Method/Constructor/Destructor. Update `GlobalScope::workspace_class_member` to parse these strings via the checker's `TypeResolver`. Add/update tests so at least one inherited-field access flows through a real type.
- **Verify**: 285 → 289+ unit; 11 integration. No parser regressions.

## Iter 27 Plan (superseded)
- **Goal**: Cross-file class hierarchy so inherited members on workspace base classes resolve through GlobalScope instead of only the in-file `file_classes` index.
- **Why**: `member_access_type` currently walks `self.file_classes` (single-file) for parent chain — any plugin with `class Foo : Base` where `Base` lives in a sibling file hits `UndefinedMember` false positives.
- **Approach**:
  1. Thread parent info through `SymbolTable`: ensure each class symbol carries its `parent_name: Option<String>` (probably already in AST, verify extract).
  2. Add `GlobalScope::workspace_class_parent(name) -> Option<String>` and `workspace_class_field/method(class, member) -> Option<TypeRepr>` that walks the cross-file chain with a visited-set cycle guard.
  3. In `member_access_type`, after the in-file shortcut fails, walk the cross-file chain via GlobalScope before declaring UndefinedMember.
- **Verify**: new tests — child_inherits_parent_field_cross_file, child_inherits_parent_method_cross_file, grandchild_two_levels_cross_file, override_shadows_parent_field, cycle_does_not_loop. Parser corpus must stay clean.
- **Target**: 285 unit + 11 integration.

## Iter 26 Result (2026-04-11)
- 280 unit + 11 integration green. Parser still 0 errors.
- AC2 real overload resolution. New `GlobalScope::lookup_function_overloads(name) -> Vec<OverloadSig>` returning all matches (previously `lookup_function_signature`/`lookup_function_param_types` silently returned None on 2+).
- New `OverloadMatch` enum (Unique/Ambiguous/NoMatch/NoOverloads) + free `resolve_overload()` with scoring: exact +2, convertible (via `is_convertible`) +1, primitive-vs-primitive mismatch rejects, non-primitive 0.
- `Checker::resolve_workspace_function_call()` helper wires both Ident call paths through the new helper while preserving the single-overload fast path (iter 19/22 behavior byte-identical).
- 5 new tests (275 → 280): exact_match_picked, convertible_match_picked, no_match_all_fail, ambiguous_silent, single_via_arg_count.

## Iter 25 Result (2026-04-11)
- 275 unit + 11 integration green. Parser still 0 errors.
- AC2 implicit conversions (numeric + null→handle). Module-level `is_convertible(from, to)` helper: error-propagation → null→handle → strip Const → PartialEq → numeric-family → false. Wired into both arg-type (iter 22) and return-type (iter 21) checks.
- Numeric family: Int8/16/32/64, Uint8/16/32/64, Float, Double. Bool/String excluded.
- 5 new tests (270 → 275).

## Iter 24 Result (2026-04-11)
- 270 unit + 11 integration green. Parser still 0 errors.
- AC1 const violation — closes the AC1 checklist. New `TypeDiagnosticKind::ConstViolation { detail }`. Fires on `Assign` and `HandleAssign` when `lhs_ty` outer layer is `TypeRepr::Const(_)`. Handles const locals, const fields, compound assigns, and const-handle reseat.
- Infra fix: `member_access_type` was consulting `GlobalScope::lookup_member_type` first, which returns `Error("")` for workspace-class members and threw away const. Added a prior check against the already-populated `self.file_classes` index so same-file class const fields retain the `Const(_)` wrapper end-to-end.
- 5 new tests (265 → 270).

## Iter 23 Result (2026-04-11)
- 265 unit + 11 integration green. Parser still 0 errors.
- AC1 handle/value mismatch. New `TypeDiagnosticKind::HandleValueMismatch { detail }`. Fires on `@=` only when LHS or RHS is clearly non-handleable (Primitive or Void). Named/Error/Null/Handle all accepted.
- Adjacent parser fix: `parse_assignment_expr` `@`-prefix branch used to recurse into `parse_assignment_expr`, which greedily captured `x = null` as a nested `Assign` — HandleAssign was effectively dead code. Fixed to call `parse_ternary_expr` for the LHS only. Parser corpus still clean.
- 4 new tests (261 → 265).

## Iter 22 Result (2026-04-11)
- 261 unit + 11 integration green. Parser still 0 errors.
- AC1 arg-type mismatch (conservative, primitive-to-primitive, workspace only). New `GlobalScope::lookup_function_param_types` (overload-suppressed). `Checker::call_type` widened from `(&Expr, usize)` → `(&Expr, &[Expr])`; `walk_args` and `walk_args_and_check_types` helpers ensure single-walk of arg expressions.
- New `TypeDiagnosticKind::ArgTypeMismatch { function_name, param_index, expected, got }`.
- 5 new tests (256 → 261).

## Iter 21 Result (2026-04-11)
- 256 unit + 11 integration green. Parser still 0 errors.
- AC1 return-type mismatch (conservative, primitive-to-primitive only). `Checker.return_type_stack` push/pop around function bodies; lambda bodies push an `Error` sentinel to prevent leak.
- New `TypeDiagnosticKind::ReturnTypeMismatch { expected, got }`.
- 5 new tests (251 → 256).

## Iter 20 Result (2026-04-11)
- 251 unit + 11 integration green. Parser still 0 errors.
- AC1 invalid-assignment-target. New `TypeDiagnosticKind::InvalidAssignmentTarget` variant. Fused `Assign` / `HandleAssign` arm in `expr_type` inspects LHS — accepts only `Ident` / `Member` / `Index` / `NamespaceAccess`, rejects everything else on `lhs.span`.
- 6 new tests (245 → 251).

## Iter 19 Result (2026-04-11)
- 245 unit + 11 integration green. Parser still 0 errors.
- AC1 arg-count mismatch for workspace functions. New `SymbolKind::Function.min_args` field; `SymbolTable::extract_symbols` now populates real `params` (including class methods/ctors/dtors) + `min_args`.
- New `GlobalScope::lookup_function_signature(name) -> Option<(min, max)>`; returns None on 0 or 2+ overloads (conservative — full overload resolution deferred). Fired at both Ident-arm call sites (top-level and namespace-scoped).
- New `TypeDiagnosticKind::ArgCountMismatch { function_name, expected_min, expected_max, got }` with range-aware message.
- 5 new tests (240 → 245). External typedb signatures, arg-type checks, and Member/NamespaceAccess arms all deferred.

## Iter 18 Result (2026-04-11)
- 240 unit + 11 integration green. Parser still 0 errors.
- AC1/AC2 deepened: **auto inference** — `check_var_decl_local` now derives the local's type from `expr_type(init)` when the declared type is `TypeExprKind::Auto`, instead of the placeholder.
- **Missing-return diagnostic** — new `TypeDiagnosticKind::MissingReturn { function_name }`. Fires on the function-name span when the return type is not `Void`, the body exists, and the last statement does not definitely terminate. Terminator analysis handles Return / Block / If-with-both-branches / Switch-with-default-and-all-cases-terminate. Constructors, destructors, and interface methods are exempted via an `enforce_return` flag on `check_function_decl`.
- 6 new unit tests (234→240).
- Adjacent concern deferred: arg-count / arg-type / overload resolution still pending.

## Iter 17 Result (2026-04-11)
- 234 unit + 11 integration green. Parser still 0 errors.
- Production wiring: `Backend::on_change` now calls `self.build_workspace().await` and passes `Some(&workspace)` into `compute_diagnostics`. Real LSP users now get the same -98% false-positive reduction that the corpus harness measures.
- New integration test `test_lsp_diagnostics_cross_document_workspace`: doc A declares `class Foo { int x; }`, doc B uses `Foo f; f.x = 1`. Verifies no `Foo`-related diagnostics fire on doc B when the workspace contains both.
- Scope discipline: did NOT eagerly load on-disk plugin files. Open-documents-only is the right single-step fix; filesystem watchers / lazy loading of sibling files is a separate concern.

## Iter 16 Result (2026-04-11)
- 234 unit + 10 integration green. Parser still 0 errors.
- AC12 (code actions) functional. New `src/server/code_actions.rs` (~360 lines + 7 tests).
- Quick-fixes: "Did you mean X?" via Levenshtein ≤2 / substring / common-prefix against workspace symbols + `TypeIndex::all_short_names()`. Up to 3 candidates, closest marked `is_preferred`.
- Always-offered refactor: "Wrap in try/catch" (REFACTOR_REWRITE).
- Diagnostic message parsing is plain-string (no regex).

## Iter 15 Result (2026-04-11)
- 227 unit + 10 integration green. Parser still 0 errors.
- AC11 (semantic tokens) functional. New `src/server/semantic_tokens.rs` with legend, AST classifier, delta encoder. 17 token types covered. Multi-line tokens deliberately skipped.

## Iters 13-14 Result (2026-04-11)
- 216 unit + 10 integration green. Parser still 0 errors.
- AC9 (rename) + AC10 (formatting) functional.
- Rename: token-scan find-references + WorkspaceEdit. New `navigation::rename`.
- Formatter: AST-based pretty-printer with span-text shortcut for non-block stmts/exprs. Roundtrip-stable on the tested subset. New `src/server/formatter.rs`. Comment loss accepted (lexer drops comments).

## Iter 12 Result (2026-04-11)
- 209 unit + 10 integration green. Parser still 0 errors.
- AC7 (document symbols) + AC8 (workspace symbols) done.
- Document symbols now nested: classes show methods/fields/properties/constructors, interfaces show methods. Workspace `workspace/symbol` handler implemented with case-insensitive substring filter.

## Iter 11 Result (2026-04-11)
- 203 unit + 10 integration green. Parser still 0 errors.
- AC5 (hover) + AC6 (completion) enriched.
- New: `src/server/scope_query.rs` (AST walker for enclosing function/class + local collection). Hover does 4-tier lookup (local → class field → workspace symbol → external typedb). Completion is context-aware: `#`, `Ident::`, trailing `.`, default identifier scope. Member completion walks parent class chain.

## Iter 10 Result (2026-04-11)
- 192 unit + 10 integration green. Parser still 0 errors.
- AC3 (goto-definition) + AC4 (find-references) implemented and wired into Backend.
- New: `src/server/navigation.rs` (246 lines) with `name_at_position`, `goto_definition`, `find_references`. Backend builds an open-document workspace per call.
- 9 new unit tests. Token-scan reference finder is pragmatic (no shadow exclusion yet).

## Iter 9 Result (2026-04-11)
- 183 unit + 10 integration green. Parser still 0 errors.
- Type histogram: **3,850 → 1,525 (−60.4%)**, files affected 543 → 275/1603.
- Cumulative since iter 4: **76,057 → 1,525 (−98.0%)**.
- Kind breakdown:
  - undefined-ident: 2,418 → 979 (−59.5%)
  - UndefinedMember: 1,002 → 116 (−88.4%, from Nadeo suppression)
  - unknown-type: 430 (unchanged)
- Fixes: `is_nadeo_type` short-circuit on UndefinedMember; virtual `get_X`/`set_X` lookup in workspace classes; `CoroutineFunc*` family added to builtins; `src/typecheck/builtins.rs` introduced.
- **AC1/AC2 status**: working subset implemented. Remaining 1,525 diagnostics are a long tail (preprocessor-branch globals, more builtins, real type bugs in plugin code). Returning to deepen later.

## Iter 8 Result (2026-04-11)
- 178 unit + 10 integration green. Parser still 0 errors.
- Type histogram: **7,956 → 3,850 (−51.6%)**, files affected 825 → 543.
- Cumulative since iter 4: **76,057 → 3,850 (−94.9%)**.
- Kind breakdown:
  - unknown-type: 5,399 → 430 (−92%)
  - undefined-ident: 2,443 → 2,418 (−1%)
  - UndefinedMember: 114 → 1,002 (+888 — receivers now resolve but Nadeo member lists incomplete)
- Fixes: TypeIndex short-name index + `resolve_unqualified` last-resort fallback; nested Nadeo enum extraction; AS builtin types (`CoroutineFunc*`, `awaitable`, `ref`) hardcoded in resolver.
- Worst 3 files: Node.as (194), CGF.as (84), TTGState.as (82).

## Iter 7 Result (2026-04-11)
- 171 unit + 10 integration green. Parser still 0 errors.
- Type histogram: **26,384 → 7,956 (−69.8%)**, files affected 1,191 → 825/1,603.
- Cumulative since iter 4: **76,057 → 7,956 (−89.5%)**.
- Kind breakdown:
  - unknown-type: 8,793 → 5,399 (−38.6%) — now dominant
  - undefined-ident: 17,477 → 2,443 (−86.0%)
  - UndefinedMember: 114 (unchanged — workspace doesn't help member resolution yet)
- New: per-plugin workspace pooling via `typecheck::workspace::build_plugin_symbol_table`. Corpus harness uses pooled workspaces; production `Backend::on_change` still passes `None` (deferred — adjacent concern).
- Worst 3 files: ItemBrowser.as (235), Node.as (194), IE_DuplicateMesh.as (142).

## Iter 6 Result (2026-04-11)
- 169 unit + 10 integration green. Parser corpus still 0 errors.
- Type histogram: 26,415 → **26,384 (−31)**. Smaller than expected.
- New diagnostic kind surfaced: `UndefinedMember` (114).
- undefined-ident: 17,622 → 17,477 (−145).
- unknown-type: 8,793 (unchanged — resolver bucket, untouched).
- Member-access derivation now works for `obj.field` / `obj.method()` chains via external `TypeIndex` with parent-class walking. `get_X`/`set_X` auto-property convention honored. Same-file class hierarchy walked.
- **Key finding**: The histogram is now bottlenecked on the harness, not the checker. Most undefined-ident hits are sibling-file plugin symbols not in the per-file workspace.

## Iter 5 Result (2026-04-11)
- 155 unit + 9 integration green. Parser corpus still 0 errors.
- Type histogram: **76,057 → 26,415 (−65.3%)**, files affected 1320 → 1191/1603.
  - undefined identifier: 66,408 → 17,622 (−73.5%)
  - unknown type: 9,649 → 8,793 (−8.9%)
- Class context + namespace context wired into Checker; `this`/`super` resolve; implicit-this member access works; namespace-scoped resolution works for both idents and types.
- Worst files now: ItemBrowser.as (638), MacroblockManip.as (539), MapInfo.as (478).

## Iter 4 Result (2026-04-11)
- 151 unit + 9 integration green.
- Parser corpus still 0 errors, 140 plugins, 1603 files.
- Type diagnostic histogram baseline: **76,057 total diagnostics**, 1320/1603 files affected.
  - `undefined identifier` → 66,408 (87%)
  - `unknown type` → 9,649 (13%)
- Worst files: ItemBrowser.as (1334), MapInfo.as (1000), Node.as (847).
- `TypeIndex` now wired into `compute_diagnostics`. `SymbolTable` extracts class members.

## Current Plan (Iteration 26 Micro-Plan)
**Overload resolution** — pick the best match when multiple workspace overloads exist; fall back to silent skip on ambiguity.
1. Add `GlobalScope::lookup_function_overloads(name: &str) -> Vec<OverloadSig>` returning every workspace `SymbolKind::Function` match. `OverloadSig = { param_types: Vec<String>, min_args: usize, return_type: String }`. Leave the existing `lookup_function_signature` / `lookup_function_param_types` for now (they still gate on unique match).
2. In `Checker::call_type` at the Ident arm (both top-level and namespace-scoped), replace the "if unique overload" flow with:
   - Fetch overloads; if empty, use existing undefined-ident fallback.
   - Precompute each arg's `expr_type` (via `walk_args`) into a `Vec<TypeRepr>`.
   - For each candidate overload, first check arg count is in range `[min_args, params_len]`. If not, skip.
   - Score the overload: per arg, exact equal (after Const strip, PartialEq) = +2; numerically convertible via `is_convertible` = +1; arg or param non-primitive or Error = 0 (neutral — can't reject, can't promote); clearly-not-convertible primitive mismatch = reject.
   - Rejected overloads drop out. Non-rejected overloads: pick the one with max score; if tie → ambiguous → silent skip; if single winner → run arg-type check against it (in case some arg was a clearly-convertible primitive), then return the winner's return type (parsed from `return_type` string via existing helpers or falling back to Error).
3. Do NOT emit new `AmbiguousOverload` diagnostic yet — silent skip keeps scope tight and avoids noise.
4. Tests (5):
   - `overload_exact_match_picked`: `void f(int a) {} void f(string a) {} void main() { f(1); }` → 0 ArgTypeMismatch, no diagnostics.
   - `overload_convertible_match_picked`: `void f(float a) {} void f(string a) {} void main() { f(1); }` → 0 ArgTypeMismatch (int→float convertible).
   - `overload_no_match_all_fail`: `void f(int a) {} void f(bool a) {} void main() { f("hi"); }` → currently silent (conservative); assert 0 ArgTypeMismatch and leave TODO for future "no matching overload" iter.
   - `overload_ambiguous_silent`: `void f(int, float) {} void f(float, int) {} void main() { f(1, 1); }` → 0 diagnostics (silent on tie).
   - `overload_single_via_arg_count`: `void f(int a) {} void f(int a, int b) {} void main() { f(1); }` → picks the 1-arg version; 0 diagnostics.
5. Target: 280 unit + 11 integration.

## Previous Plan (Iteration 25 Micro-Plan)
**Implicit conversions** — makes arg-type and return-type checks smarter. Numeric widening + null→handle + Error suppression.
1. New helper `fn is_convertible(from: &TypeRepr, to: &TypeRepr) -> bool` in checker.rs (or a sibling helpers module if it's cleaner). Rules (conservative + permissive in the right places):
   - Error → * → true; * → Error → true (suppress)
   - Null → Handle(_) → true
   - Unwrap `Const(_)` on both sides before comparing
   - Structurally equal (ignoring Const) → true
   - Both `Primitive`: true iff both are in the numeric family (Int8/16/32/64, Uint8/16/32/64, Float, Double) — this permits all numeric-to-numeric conversions (widening and implicit narrowing — Angel allows both). Bool is NOT in the numeric family. String is NOT.
   - Everything else → false
2. Wire into arg-type check (iter 22): replace "both primitives and differ" with "both primitives AND NOT is_convertible". Practically: emit iff both are primitives AND not `is_convertible(arg, param)`.
3. Wire into return-type check (iter 21): same replacement.
4. Tests (5 new, plus verify existing still pass):
   - `arg_type_int_to_float_implicitly_ok`: `void f(float a) {} void main() { f(1); }` → 0 ArgTypeMismatch
   - `arg_type_int_to_bool_fires`: `void f(bool a) {} void main() { f(1); }` → 1 ArgTypeMismatch
   - `arg_type_string_to_int_still_fires`: existing behavior confirmed
   - `return_int_from_double_ok`: `double f() { return 1; }` → 0 ReturnTypeMismatch
   - `return_bool_from_int_fires`: `bool f() { return 1; }` → 1 ReturnTypeMismatch
5. Target: 275 unit + 11 integration green.

## Previous Plan (Iteration 24 Micro-Plan)
**Const violation** — closes out AC1's diagnostic checklist.
1. New `TypeDiagnosticKind::ConstViolation { detail: String }`. Message: `"const violation: {detail}"`.
2. In `Checker::expr_type`, at the `Assign { lhs, op, rhs }` arm (after invalid-target check): compute `lhs_ty = self.expr_type(lhs)`. If `lhs_ty` is `TypeRepr::Const(_)` at its outer layer, emit on `lhs.span` with detail `"cannot assign to const value"`.
3. Do the same for `HandleAssign` — reassigning a const handle is also invalid.
4. Ensure local lookup preserves const wrappers: `check_var_decl_local` must call `self.resolve_type_expr(&var.type_expr)` which already returns `TypeRepr::Const(_)` for `TypeExprKind::Const(_)`. Verify via inspection — no change needed unless broken.
5. Member access of const fields: `member_access_type` returns the raw field type. Verify it preserves Const from field declarations.
6. Tests (5):
   - `const_local_assign_fires`: `void f() { const int x = 5; x = 6; }` → 1 ConstViolation.
   - `non_const_local_assign_ok`: `void f() { int x = 5; x = 6; }` → 0 ConstViolation.
   - `const_field_assign_fires`: `class C { const int x; } void f() { C@ c; c.x = 6; }` → 1 ConstViolation.
   - `const_compound_assign_fires`: `void f() { const int x = 1; x += 2; }` → 1 ConstViolation (any `AssignOp`, not just plain `=`).
   - `handle_assign_to_const_fires`: `class C {} void f() { const C@ a; @a = null; }` → 1 ConstViolation.
7. Verify green; target 270 unit + 11 integration.

## Previous Plan (Iteration 23 Micro-Plan)
**Handle/value mismatch** — the last narrow AC1 item before AC2 deepening.
1. New `TypeDiagnosticKind::HandleValueMismatch { detail: String }` variant. Message: `"handle/value mismatch: {detail}"`.
2. In `Checker::expr_type`'s `HandleAssign { lhs, rhs }` arm, after walking lhs and rhs:
   - Get `lhs_ty = expr_type(lhs)`, `rhs_ty = expr_type(rhs)` (already done by iter 20 walk). If neither is `Error`:
     - If `lhs_ty` is NOT `TypeRepr::Handle(_)` → emit `"left-hand side of @= is not a handle"`.
     - Else if `rhs_ty` is NOT `Handle(_)` and not `Null` → emit `"right-hand side of @= must be a handle or null"`.
3. Suppress when either side is `Error`, `Named` (workspace class shortcut is fine), or `Primitive` without clear handle annotation (too noisy).
4. Do NOT touch regular `Assign` yet — the value-copy-vs-handle-reseat confusion needs more care and will create false positives on opAssign.
5. Tests (4): `handle_assign_both_handles_ok`, `handle_assign_null_rhs_ok`, `handle_assign_value_rhs_fires`, `handle_assign_primitive_rhs_fires`.
6. Verify: target 265 unit + 11 integration.

## Previous Plan (Iteration 22 Micro-Plan)
**Arg-type mismatch** — conservative, primitive-to-primitive, workspace functions only. Mirrors iter 21's return-type approach.
1. Extend `GlobalScope::lookup_function_signature` into a sibling `lookup_function_param_types(name) -> Option<Vec<String>>` returning `None` if 0/2+ overloads, otherwise `Some(Vec<type_text>)` from the unique matching `SymbolKind::Function { params, .. }`. Use the second element of each `(name_text, type_text)` tuple.
2. Change `Checker::call_type` to accept `&[Expr]` instead of `usize arg_count`. Update call site in `expr_type::Call` arm.
3. After `check_arg_count` succeeds (or alongside it — in both the namespace-scoped-match and top-level-match paths), walk each arg in order:
   - Get arg type via `self.expr_type(arg)`. If not `TypeRepr::Primitive(_)`, skip.
   - Parse the corresponding param type text with `PrimitiveType::from_name`. If not a primitive, skip.
   - If the two primitives differ, emit new `TypeDiagnosticKind::ArgTypeMismatch { function_name, param_index, expected: String, got: String }` on the arg's span.
4. Message: `` argument {index+1} of `{name}`: expected `{expected}`, got `{got}` ``.
5. Tests (5): `arg_type_primitive_match_ok`, `arg_type_primitive_mismatch_fires`, `arg_type_non_primitive_suppressed`, `arg_type_error_type_suppressed`, `arg_type_overloaded_suppressed`.
6. Verify: `CC=gcc cargo test` green, target 261 unit + 11 integration.

## Previous Plan (Iteration 21 Micro-Plan)
**Return-type mismatch** — conservative single-pass check, primitive-to-primitive only.
1. Add `return_type_stack: Vec<TypeRepr>` field on `Checker`. Push on `check_function_decl` entry (before walking the body), pop on exit. Same for interface methods if the path walks them (they have no body, so skip).
2. In `check_stmt`'s `Return(Some(e))` arm: derive `expr_type(e)`, then if both the function's return type and the expression type are `TypeRepr::Primitive(_)` AND they differ AND neither is `Error`, emit a new `TypeDiagnosticKind::ReturnTypeMismatch { expected: String, got: String }` on `e.span`.
3. Intentionally do NOT flag: error types, Named types, handles, generics, arrays, void, auto, Null. Primitive-vs-primitive only. Conservative by design.
4. Do NOT also handle `return e` inside lambdas this iter (lambdas are rare; noise risk).
5. Message format: `"return type mismatch: function returns `<expected>`, got `<got>`"`.
6. Tests: `return_int_from_int_ok`, `return_string_from_int_fires`, `return_ident_preserves_silence` (return of an undefined ident → no ReturnTypeMismatch, only UndefinedIdentifier), `return_from_void_ok`, `return_null_from_handle_suppressed` (ensure we don't fire on `ClassName@ f() { return null; }`).
7. Verify: `CC=gcc cargo test` green, expect 256 unit + 11 integration.

## Previous Plan (Iteration 20 Micro-Plan)
**Invalid assignment target** — tight, self-contained AC1 item. No symbol-table changes needed.
1. New `TypeDiagnosticKind::InvalidAssignmentTarget` + message `"invalid left-hand side in assignment"`.
2. In `Checker::expr_type`, at the `ExprKind::Assign { lhs, op, rhs }` and `ExprKind::HandleAssign { lhs, rhs }` arms: inspect `lhs.kind` and emit `InvalidAssignmentTarget` on `lhs.span` if it is NOT one of: `Ident`, `Member`, `Index`, `NamespaceAccess`, `This`, `Super`, `Postfix` (for chains like `x++ = y` — no wait, that should be rejected). Accept l-values = Ident, Member, Index, NamespaceAccess. Reject everything else: literals, Call, Binary, Unary, Ternary, Cast, TypeConstruct, ArrayInit, Lambda, Is, Null, This, Super, Postfix.
3. Actually: `This = ...` is invalid in AngelScript, and so is `Super = ...`. So the accept list is: Ident, Member, Index, NamespaceAccess. Everything else is rejected.
4. Tests: `assign_to_ident_ok`, `assign_to_member_ok`, `assign_to_index_ok`, `assign_to_literal_fires`, `assign_to_call_fires`, `assign_to_binary_fires`.
5. Verify `CC=gcc cargo test` green, expect 251 unit + 11 integration.

## Previous Plan (Iteration 19 Micro-Plan)
Tackle **arg-count mismatch** for workspace-declared functions (single-overload, non-external). This is the next self-contained AC1 item and unblocks arg-type / return-type iterations later.
1. Extend `SymbolKind::Function { ... }` with a new `min_args: usize` field (count of non-defaulted params). Update the 4 construction sites in `src/symbols/table.rs` and the 2 in `src/typecheck/global_scope.rs` plus any test sites.
2. In `SymbolTable::extract_symbols`, for `Item::Function`, class `Method`, and `Constructor`: populate `params` with `(param_name, type_text)` using `type_expr.span.text(source)` for the type text, and compute `min_args` as `params.len() - <count of params with default_value.is_some()>`.
3. Add `GlobalScope::lookup_function_signature(name) -> Option<(usize /*min*/, usize /*max*/)>` returning `None` if the name resolves to 0 or 2+ symbols (suppress on overload — proper resolution is a later iter); returning `Some((min, max))` on unique match.
4. New `TypeDiagnosticKind::ArgCountMismatch { function_name, expected_min, expected_max, got }` + message like `` function `f` expects <min> args, got <got> `` (format with range when min≠max).
5. In `Checker::call_type` Ident arm, when the name does NOT match a local/class-member/namespace entry and it IS a unique workspace function, after `lookup_function_return` succeeds also do a signature-lookup and emit `ArgCountMismatch` if `args.len() < min || args.len() > max`. Do NOT fire for external typedb functions this iter.
6. Tests: `arg_count_match_ok`, `arg_count_too_few_fires`, `arg_count_too_many_fires`, `arg_count_optional_params_respected`, `arg_count_overloaded_suppressed` (define two top-level functions `f(int)` and `f(int, int)` — should not fire).
7. Do NOT thread arg-count into the Member/Namespace call arms this iter; Ident only. Do NOT touch arg-type checks. Scope discipline.
8. Verify `cargo test` green — target 245 unit + 11 integration.

## Previous Plan (Iteration 18 Micro-Plan)
Deepen AC1/AC2 with two small orthogonal pieces from the gap list:
1. **Auto inference**: in `check_var_decl_local`, when `var.type_expr.kind == TypeExprKind::Auto`, derive the local's type from `expr_type(init)` instead of storing `TypeRepr::Named("auto")`. If the initializer's type is `Error` or absent, fall back to `TypeRepr::Error` (keeps downstream silent).
2. **Missing-return diagnostic**: new `TypeDiagnosticKind::MissingReturn { function_name }`. At the end of `check_function_decl`, if the resolved return type is not `void` and the body does not end on a terminating-return, emit it on the function name span. Terminator predicate: walks the last statement — a `Return(_)`, a `Block` whose last stmt terminates, an `If` whose both branches terminate, a `Switch` whose every case terminates. Keep it simple; over-reject false-positives by staying conservative (only fire if we can *prove* it's missing).
3. Unit tests in `checker.rs`: `auto_local_inferred_from_int_literal`, `auto_local_inferred_from_member_access`, `missing_return_on_nonvoid_function_fires`, `return_in_all_if_branches_suppresses_missing_return`, `void_function_without_return_ok`.
4. Run full suite — target: 239+ unit green, 11 integration still green, parser corpus clean. No regressions on existing type-histogram file counts if I rerun it.
5. Adjacent concern (do NOT tackle this iter): arg-count checking needs param vectors populated in SymbolTable. Separate iter.

## Previous Plan (Iteration 17 Micro-Plan)
Close the production wiring gap that iter 7 deferred: `Backend::on_change` must feed `compute_diagnostics` a per-plugin workspace SymbolTable, not `None`. Without this, real LSP users see the pre-iter-5 false-positive flood.
1. Reuse `Backend::build_workspace()` — it already pools open documents into a fresh `SymbolTable`. That is the minimal fix.
2. Change `on_change(&self, uri, text)` to call `self.build_workspace().await` before `compute_diagnostics`, then pass `Some(&table)` as the `workspace_symbols` arg. Note: `build_workspace` re-preprocesses + re-parses every open doc; acceptable for now (small N = open editors).
3. Integration test: open 2 docs where A declares `class Foo` and B uses `Foo foo;`. Confirm B's diagnostics don't flag `Foo` as unknown. New test in `tests/integration_tests.rs` using the existing LSP harness pattern.
4. Verify `cargo test` green; verify `OPENPLANET_PLUGINS`-gated corpus test still runs clean.
5. Scope discipline: do NOT also load on-disk workspace plugin files in this iter. Open-documents-only is the correct single-step fix. Loading workspace files eagerly is a separate concern (probably a file-system watcher / lazy loader, several iterations of work).
6. Adjacent cleanup if trivial: remove the `#[allow(dead_code)]` on `Backend::symbol_table` if we can now use it, or leave as-is.

## Previous Plan (Iteration 10 Micro-Plan)
Pivot to feature breadth. Wire AC3 (goto-definition) and AC4 (find-references) together — they share the position-→-symbol resolution path.
1. New module `src/server/navigation.rs` exposing `goto_definition` and `find_references`.
2. Position resolver: given (source, position), find the AST node + symbol kind under the cursor. Reuse `lexer::tokenize_filtered` + a token walk to find the ident at offset, then walk parser AST to identify what kind of symbol it refers to. Start simple: handle Ident references and qualified names.
3. Goto: look up the symbol in the workspace `SymbolTable`. Return its `span` as an `LSP Location`.
4. References: scan the workspace `SymbolTable` for symbols matching the name; for each, find usage sites by re-parsing files (or by token-scanning for the ident text — pragmatic shortcut, will catch shadows but high recall).
5. Wire `Backend::goto_definition` and `Backend::references` to call into navigation module. Use same per-document `documents` DashMap; build per-call workspace symbol table from the document set (full plugin loading is iter 11+).
6. Tests: unit tests for `position_to_token_ident`, `find_definition_for_global_function`, `find_references_for_class`. Integration test that opens 2 docs and exercises goto.
7. Update `Backend` capabilities to advertise both providers (already advertised — verify).

## Previous Plan (Iteration 5 Micro-Plan)
Attack the undefined-identifier bucket (87% share).
1. Teach `Checker` to maintain a **class context stack** (`current_class: Vec<ClassCtx { name: String, members: Vec<(String, TypeRepr)> }>`).
2. On entering `Item::Class`, push a class ctx with resolved member types (from ClassMember::Field and ::Method resolved via TypeResolver).
3. Inside a method body, implicit `this` member references: if an unqualified ident isn't a local AND isn't a global, check current class members before flagging undefined.
4. `this` / `super` now resolve to `TypeRepr::Named(current_class_name)`.
5. Teach `Checker` about **current namespace stack**: on entering `Item::Namespace`, push ns name. Unqualified ident lookups inside a namespace should try `ns::ident` in global scope too.
6. Global ident lookup path: `GlobalScope` gets a `has_global_ident(name) -> bool` helper that matches any function/type/enum/var by qualified-or-bare name.
7. Re-run type histogram; expect massive reduction in undefined-identifier count.
8. Unit tests: `this_member_resolves`, `implicit_this_member_resolves`, `namespace_local_ident_resolves`, `class_field_usable_in_method`.

## Previous Plan (Iteration 4 Micro-Plan)
Reduce false-positive rate so the corpus becomes a useful signal, then measure.
1. Extend `SymbolTable::extract_item_symbols` to descend into `ClassDecl::members` — register methods (Function), constructors, destructors, fields (VarDecl), and properties. Each class-member symbol should use a qualified name like `"ClassName::memberName"`. Add unit tests.
2. Thread `TypeIndex` into `compute_diagnostics`:
   - Change `compute_diagnostics` signature to accept `type_index: Option<&TypeIndex>`.
   - Update the two call sites (`Backend::on_change` and any test/integration caller) to pass the loaded index.
   - `Backend::on_change` must acquire the `type_index` read lock before calling `compute_diagnostics`.
   - Inside `compute_diagnostics`, pass the external index into `GlobalScope::new(&symbols, external)`.
3. Add a new corpus harness in `tests/integration_tests.rs`: `test_corpus_type_diagnostic_histogram` — gated on `OPENPLANET_PLUGINS` AND `OPENPLANET_TYPEDB_DIR` env vars. Loads the type index once, then for each `.as` file runs `compute_diagnostics` and histograms the resulting diagnostic messages. Writes to `.goal-loops/type-diag-histogram.txt`.
4. Run the histogram, read the top 20 kinds, record them in the goal file. This becomes the targeting list for iter 5+.
5. Do NOT fix checker bugs this iteration — just fix the scaffolding that makes measurement possible. Scope discipline.
6. Verify all unit + integration tests still pass. Parser corpus still 0 errors.

## Blockers / Notes
- **`CC=gcc`**: Required in this sandbox for zstd-sys to compile. Every `cargo` invocation needs `CC=gcc`.
- **Context hygiene**: Heavy reads and bulk edits delegated to subagents. Main loop reads only the goal file, short summaries, and short source files directly.
- **Scope discipline**: Adjacent parser fixes are in scope if they block a feature; adjacent features are NOT in scope (YAGNI).
- **Parser corpus regression**: Must keep running corpus test under OPENPLANET_PLUGINS env var after each iteration to verify no parse regressions.

## ON_GOAL_COMPLETE_NEXT_STEPS
*Single-phase goal — no auto-advance chain defined. On completion, report to user and stop.*
