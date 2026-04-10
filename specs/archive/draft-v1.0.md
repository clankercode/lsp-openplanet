# OpenPlanet LSP — Specification

**Status:** draft-v1.0 (post-review, all findings addressed)
**Stable path:** `specs/draft-v1.md`
**Archive:** `specs/archive/` | index: `.7/spec-drafting/archive-index.md`

## Source Inputs

- `~/scrape/openplanet/` — ~460 scraped OpenPlanet doc files (API subset, reference, tutorials).
  **Note:** The scrape covers the `global` namespace, `nvg`, `mat3/4`, `string`, and
  reference pages. Most API namespaces (`UI::`, `Net::`, `Math::`, etc.) are NOT in the
  scrape — they come from the game-specific JSON type files.
- `~/scrape/angelcode/` — **Not available on disk.** AngelScript base language syntax in
  Section 5.1 is derived from the existing VS Code extension's grammar, real plugin code
  patterns, and general AngelScript knowledge. Claims about base AngelScript syntax should
  be verified against the official AngelScript documentation during implementation.
- `~/src/openplanet/` — 154 real TM plugin source trees (ecosystem survey)
- `~/src/openplanet/vscode-openplanet-angelscript/` — existing VS Code LSP (reference for
  JSON schema format and feature scope only; not forking)
- `~/src/openplanet/tm-scripts/` — `OpenplanetCore.json` and `OpenplanetNext.json` type
  database files
- User input (this conversation)

**Source basis note:** Primary source evidence comes from scraped OpenPlanet developer docs, real plugin codebases, and the type JSON files. The existing VS Code LSP is used as a reference for JSON schema formats, but not as an architectural influence. AngelScript base language syntax lacks a scraped reference — verify against official docs during implementation.

---

## Section Status Table

| # | Section | Status | Notes |
|---|---------|--------|-------|
| 1 | Problem and goals | reviewed | Foundation confirmed Round 1 |
| 2 | Scope + non-goals | reviewed | All blocking decisions resolved |
| 3 | Users and actors | reviewed | Straightforward from scope |
| 4 | Core workflows | reviewed | Config resolved (D-008, D-009) |
| 5 | Concrete syntax (AngelScript/OpenPlanet) | reviewed | Comprehensive from source |
| 6 | Domain-specific semantics | reviewed | Type system, preprocessor, resolution |
| 7 | Functional requirements | reviewed | 37 requirements (FR-01..FR-37) |
| 8 | Data and state | reviewed | Type DB, symbol table, deps, preprocessor, docs |
| 9 | Interfaces and integrations | reviewed | LSP, JSON, TOML, filesystem, Rust crates |
| 10 | UX / operator experience | reviewed | Perf targets, degraded mode, commands |
| 11 | Constraints | reviewed | Rust, perf, protocol, compat, testing |
| 12 | Edge cases and failure modes | reviewed | Parsing, type resolution, deps, config |
| 13 | Acceptance criteria | reviewed | Fixture suite (AC-01..AC-10) |
| 14 | Assumptions / deferred / unresolved | reviewed | 9 assumptions, 14 confirmed, 6 deferred |
| 15 | Provenance and evidence | reviewed | 5 evidence sources documented |
| 16 | Risks | reviewed | 6 risks with mitigations |

---

## Decision Notes (summary)

- **D-001** Confirmed (user): Implementation language is **Rust**. Design priorities: fast, accurate, low memory/CPU.
- **D-002** Confirmed (user): **Fresh build**. Existing VS Code LSP is "algorithmically bad" — reference only, not a fork target.
- **D-003** Confirmed (user): **Generic stdio LSP** — editor-agnostic, works with any LSP client.
- **D-004** Confirmed (user): Real plugin source files + API docs used as test fixtures; all errors must be accounted for.
- **D-005** Confirmed (user): Primary game target **TMNEXT**. Type info JSON files share the same format across all games — LSP should support swapping game target. Preprocessor `#if` handling is the key difficulty.
- **D-006** Confirmed (user): **Web Services API is out of scope** — no native support in OpenPlanet AngelScript.
- **D-007** Confirmed (user): **info.toml must be parsed by the LSP** with error feedback (diagnostics on the TOML itself).
- **D-008** Confirmed (user): Layered configuration: auto-detect → config file → LSP init params.
- **D-009** Confirmed (user): Default defines: all-permissive (all game targets + platform + SIG_DEVELOPER).

---

## 1. Problem and Goals

**Status:** reviewed | Provenance: Confirmed (source) + Confirmed (user)

### Problem

OpenPlanet plugin developers write AngelScript code targeting a large, evolving game API
(TrackMania / ManiaPlanet). The development experience lacks a fast, accurate language server:

- AngelScript is not a mainstream language — no general-purpose language servers cover it.
- OpenPlanet extends AngelScript with a custom preprocessor, plugin lifecycle callbacks,
  attribute syntax (`[Setting ...]`), coroutine primitives (`startnew`, `yield`),
  and ~25 API namespaces spanning 500+ documented types.
- Plugins are multi-file modules; cross-file type resolution is necessary.
- Dependencies between plugins (via `info.toml` exports/imports) create additional
  resolution scope.
- An existing VS Code extension (`vscode-openplanet-angelscript`) exists but is
  algorithmically poor — slow, resource-heavy, and not a good foundation to build on.

### Goals

1. Provide accurate, context-aware code completion for OpenPlanet API symbols.
2. Provide type-aware hover documentation.
3. Provide go-to-definition and find-references across plugin source files.
4. Surface diagnostics (type errors, undefined symbols, illegal API usage) against the
   actual OpenPlanet API.
5. Support the OpenPlanet preprocessor (`#if`/`#elif`/`#else`/`#endif` with game/platform
   defines) such that symbol visibility is correct per active define set.
6. Parse `info.toml` and provide diagnostics on it (malformed TOML, invalid fields, etc.).
7. Resolve plugin dependencies declared in `info.toml` and load their exported types.
8. Be validated against real TM plugin source trees and documented API; all errors must be
   accounted for via a fixture test suite.
9. Be fast, accurate, and low on memory and CPU usage — a principled Rust implementation.

---

## 2. Scope + Non-Goals

**Status:** reviewed | Provenance: Confirmed (user)

### In Scope

- **Language:** OpenPlanet's AngelScript dialect (not standard AngelScript, not Unreal AngelScript).
- **Implementation:** Rust, fresh build. No fork of existing tools.
- **Transport:** Generic LSP over stdio. Editor-agnostic.
- **Primary game target:** TMNEXT (TrackMania 2020).
- **Game target swapping:** The type info JSON files (`OpenplanetCore.json`, `OpenplanetNext.json`)
  share the same format across all game targets. The LSP should accept a game target configuration
  and load the corresponding type database. Swapping game targets should be straightforward.
- **Preprocessor:** `#if`/`#elif`/`#else`/`#endif` with C-like semantics. Primarily `#if` is used
  in practice. Defines are game/platform/signature-specific (e.g., `TMNEXT`, `DEV`, `MANIA64`,
  `SIG_DEVELOPER`). The LSP must evaluate these to determine symbol visibility.
- **info.toml:** Full parsing with diagnostic feedback. Validates structure, dependencies,
  exports, defines, and other fields.
- **Plugin dependencies:** Resolve `dependencies` and `optional_dependencies` from `info.toml`,
  locate dependency plugins, and load their `exports`/`shared_exports` for type resolution.
- **Test fixtures:** Built from real TM plugin source trees and real OpenPlanet API documentation.
  Every diagnostic the LSP emits against fixtures must be accounted for (true positive or
  documented known issue).

### Non-Goals

- **Web Services API (Nadeo REST endpoints):** No native OpenPlanet AngelScript support for these.
  Out of scope entirely.
- **Live game state introspection:** Out of scope for an LSP.
- **Code formatting / style enforcement:** Not requested. Deferred.
- **Forking or wrapping the existing VS Code extension:** Explicitly rejected.
- **Debugger / DAP integration:** Not mentioned. Deferred.
- **Plugin packaging / publishing:** Not an LSP concern.

---

## 3. Users and Actors

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs)

### Primary User

**OpenPlanet plugin developer** — writes AngelScript code in an LSP-capable editor
(VS Code, Neovim, Helix, etc.) targeting TrackMania via OpenPlanet.

Typical characteristics:
- Works in a plugin project directory containing `info.toml` and `.as` source files.
- May depend on other installed plugins via `info.toml` dependencies.
- Uses OpenPlanet API namespaces (`UI::`, `Net::`, `Math::`, `nvg::`, etc.) heavily.
- Uses preprocessor `#if` for conditional compilation across game targets.
- Expects fast feedback: completion as they type, instant diagnostics, quick navigation.

### Secondary Actors

- **Editor client:** Any LSP-capable editor communicating via stdio. The LSP makes no
  assumptions about the client beyond standard LSP protocol support.
- **OpenPlanet type database:** Pre-generated JSON files describing the game API.
  The LSP reads these at startup; they are not generated by the LSP itself.
- **Installed plugin ecosystem:** Other plugins on disk whose exports may need to be
  resolved for dependency type checking.

---

## 4. Core Workflows

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs) + Confirmed (source)

### 4.1 Workspace Initialization

1. **Root detection:** The LSP walks up from the opened file to find `info.toml`. The
   directory containing `info.toml` is the workspace root. If no `info.toml` is found,
   the LSP operates in degraded mode (single-file, no dependency resolution).
2. **Parse info.toml:** Validate TOML syntax and structure. Emit diagnostics for:
   - Malformed TOML
   - Unknown keys
   - Invalid types (e.g., `version` not a string)
   - Missing required fields (`[meta].version`)
   - Invalid dependency references
3. **Discover source files:** Enumerate all `.as` files under the workspace root.
   These form the plugin's compilation unit.
4. **Load type database:** Read `OpenplanetCore.json` and the game-specific JSON
   (e.g., `OpenplanetNext.json` for TMNEXT) from a configured path. Parse into the
   in-memory type database. This provides all built-in API types, functions, and enums.
5. **Resolve dependencies:** For each plugin listed in `dependencies`,
   `optional_dependencies`, and `export_dependencies` in `info.toml`:
   - Locate the dependency plugin on disk (configured plugin directory).
   - Parse its `info.toml` to find `exports` and `shared_exports`.
   - Parse those exported `.as` files and add their types to the resolution scope.
   - For `optional_dependencies`, if not found, define no `DEPENDENCY_X` preprocessor
     symbol (where X = plugin identifier); if found, define it.
6. **Evaluate preprocessor defines:** Determine the active define set from:
   - Game target (e.g., `TMNEXT`)
   - Platform (e.g., `MANIA64`, `WINDOWS`)
   - Signature mode (e.g., `SIG_DEVELOPER`)
   - Custom defines from `info.toml` `[script].defines`
   - Dependency-derived defines (`DEPENDENCY_X`)
7. **Parse all source files:** Lex, preprocess, and parse each `.as` file. Build the
   symbol table (types, functions, variables, namespaces) with cross-file resolution.
8. **Emit initial diagnostics:** Report all errors/warnings to the editor.

### 4.2 Incremental Update Cycle

On file change (didChange/didSave):
1. Re-lex and re-parse the changed file.
2. Update the symbol table entries for that file.
3. Re-check diagnostics for the changed file and any files that depend on symbols
   defined in it (cross-file invalidation).
4. Push updated diagnostics to the editor.

On `info.toml` change:
1. Re-parse `info.toml` and emit diagnostics.
2. If dependencies changed, re-resolve dependencies and re-parse affected exports.
3. If defines changed, re-evaluate preprocessor and re-parse all files.

### 4.3 LSP Request Handling

| Request | Behavior |
|---------|----------|
| `textDocument/completion` | Context-aware: after `::` → namespace members; after `.` → member access on resolved type; after `@` → handle-compatible types; top-level → globals, types, keywords. |
| `textDocument/hover` | Show type signature + doc string from type DB or source comments. |
| `textDocument/definition` | Jump to definition: source file location for user-defined symbols; no-op or doc link for API-defined symbols. |
| `textDocument/references` | Find all references to symbol across all files in the compilation unit. |
| `textDocument/rename` | Rename symbol across all files. Validate new name is a legal identifier. |
| `textDocument/signatureHelp` | Show parameter list and types for function/method calls. |
| `textDocument/diagnostics` | Push diagnostics: type errors, undefined symbols, preprocessor errors, info.toml errors. |
| `textDocument/documentSymbol` | Outline: classes, functions, enums, namespaces, globals in the file. |
| `workspace/symbol` | Search across all files in the workspace. |

### 4.4 Configuration

**Layered configuration** (lowest → highest priority):

1. **Auto-detect:** Try standard OpenPlanet installation paths:
   - Windows: `%USERPROFILE%/OpenplanetNext/`
   - Linux (Wine): TBD (common Wine prefix paths)
   Locate `OpenplanetCore.json`, `OpenplanetNext.json`, and the `Plugins/` directory
   from the installation.

2. **Config file:** `.openplanet-lsp.toml` in workspace root or `~/.config/openplanet-lsp/config.toml`.
   Example:
   ```toml
   openplanet_dir = "/path/to/OpenplanetNext"
   plugins_dir = "/path/to/OpenplanetNext/Plugins"
   game_target = "TMNEXT"
   defines = ["TMNEXT", "MANIA64", "WINDOWS", "SIG_DEVELOPER"]
   ```

3. **LSP initialization params:** Editor client passes overrides in `initializationOptions`.
   Same keys as the config file. Highest priority.

**Default preprocessor defines** (most-permissive set, so all code paths are visible
during development):
- All game defines: `TMNEXT`, `MP4`, `MP40`, `MP41`, `TURBO`, `FOREVER`,
  `UNITED_FOREVER`, `NATIONS_FOREVER`, `UNITED`, `MP3`
- Platform: `MANIA64`, `MANIA32`, `WINDOWS`, `WINDOWS_WINE`, `LINUX`
- Build: `SERVER`, `LOGS`, `HAS_DEV`, `DEVELOPER`
- Signature (Developer level): `SIG_OFFICIAL`, `SIG_REGULAR`, `SIG_SCHOOL`, `SIG_DEVELOPER`
- Plus any `defines` from `info.toml`
- Plus `DEPENDENCY_X` for found optional dependencies

**Note:** Enabling all game defines simultaneously means `#if TMNEXT` and `#if MP4`
are both true. This is intentional for LSP purposes — it ensures all code branches
are analyzed and errors surface everywhere. Users can narrow the define set via config
to match a specific runtime target if desired.

**Conflicting definitions from mutually exclusive branches:** When all defines are active,
`#if TMNEXT` and `#else` (or `#if MP4`) branches both produce symbols. If the same
symbol name is defined differently in both branches, the type checker must either:
- (a) Treat the last definition as authoritative (simple, may miss real errors), or
- (b) Report a diagnostic noting the conflict (stricter, may be noisy for multi-target code).
**Decision needed during implementation.** The recommended default is (a) — last-wins with
no diagnostic — since all-permissive mode is a convenience for broad analysis, not a
strict compilation mode.

---

## 5. Concrete Syntax (AngelScript / OpenPlanet Dialect)

**Status:** reviewed | Provenance: Confirmed (source)

### 5.1 AngelScript Base Syntax

The LSP must parse the full AngelScript language as used by OpenPlanet. Key constructs:

**Declarations:**
- `class Name : Base { ... }` — classes with single inheritance
- `interface Name { ... }` — interfaces
- `enum Name { A, B = 1, C }` — enumerations
- `namespace Name { ... }` — namespaces (nestable)
- `funcdef RetType FuncName(args)` — first-class function type declarations
- `mixin class Name { ... }` — mixins
- `shared class Name { ... }` — shared cross-module types

**Functions:**
- `RetType Name(args) { ... }` — global and member functions
- Default parameter values
- `const` methods
- `override`, `final` modifiers
- `property` accessors (get/set)

**Types:**
- Primitives: `void`, `bool`, `int`, `int8/16/32/64`, `uint`, `uint8/16/32/64`,
  `float`, `double`
- `string`, `wstring` (wide string, used in game API types like `MwFastBuffer<wstring>`)
- `auto` — type inference
- `T@` — object handle (nullable reference)
- `T&` — reference (with `in`, `out`, `inout` modifiers)
- `const T` — const qualification
- `array<T>` or `T[]` — arrays
- `dictionary` — associative map

**Expressions:**
- Standard arithmetic, logical, bitwise, comparison operators
- Ternary: `cond ? a : b`
- `cast<T>(expr)` — type casting (returns null handle on failure)
- `expr is null`, `expr !is null` — null checks
- Member access: `obj.member`, `Namespace::member`
- Array indexing: `arr[i]`
- Lambda / inline functions (if supported by OpenPlanet's dialect)

**Statements:**
- `if/else`, `for`, `while`, `do/while`, `switch/case/default`
- `break`, `continue`, `return`
- `try/catch` — exception handling
- Variable declarations with initialization

**Comments:**
- `//` line comments
- `/* */` block comments

### 5.2 OpenPlanet Extensions

**Preprocessor directives:**
```
#if DEFINE
#if !DEFINE
#if DEFINE1 && DEFINE2
#if DEFINE1 || DEFINE2
#if !DEFINE1 && DEFINE2
#elif DEFINE
#else
#endif
```
- Operators: `!` (negation), `&&` (and), `||` (or).
- `&&` and `||` evaluate left-to-right with no precedence rules.
- Parentheses are NOT supported for grouping.
- `!` binds to the immediately following define name.
- Known define sets: game (`TMNEXT`, `MP4`, `TURBO`, `FOREVER`, etc.), platform
  (`MANIA64`, `MANIA32`, `WINDOWS`, `LINUX`, etc.), signature
  (`SIG_OFFICIAL`..`SIG_DEVELOPER`), competition (`COMP_*`), and custom defines
  from `info.toml`.

**Metadata attributes (decorators):**
```
[Setting name="X" category="Y" min=0 max=100 drag hidden description="..."]
[Setting color]
[Setting multiline]
[Setting password]
[Setting if="BoolVar" enableif="IsValid"]
[Setting beforerender="Callback" afterrender="Callback"]
[SettingsTab name="X" icon="Y" order=N]
[Persistent hidden]
```
- Attributes precede a global variable or function declaration.
- `[Setting ...]` — creates a user-visible setting in the OpenPlanet settings panel.
  Supported types: `bool`, `int*`, `uint*`, `float`, `double`, `string`, `vec2/3/4`,
  `int2/3`, `nat2/3`, `quat`, any enum.
- `[SettingsTab ...]` — marks a function as a custom settings tab renderer.
- `[Persistent ...]` — persistent storage (not displayed in settings UI).
  **UNCONFIRMED:** No occurrences found in 154 plugin source trees or scraped docs.
  May exist in unscraped OpenPlanet documentation. Verify before implementing.

**Plugin lifecycle callbacks** (global functions with specific signatures):
- `void Main()` — entry point, yieldable (coroutine)
- `void Render()`, `void RenderInterface()`, `void RenderMenu()`,
  `void RenderMenuMain()`, `void RenderEarly()` — frame rendering callbacks
- `void RenderSettings()` — deprecated (use `[SettingsTab]` instead)
- `void Update(float dt)` — per-frame update
- `void OnEnabled()`, `void OnDisabled()`, `void OnDestroyed()` — lifecycle
- `void OnSettingsChanged()` — settings panel callback
- `void OnSettingsSave(Settings::Section& section)` — save callback
- `void OnSettingsLoad(Settings::Section& section)` — load callback
- `void OnKeyPress(bool down, VirtualKey key)` or
  `UI::InputBlocking OnKeyPress(bool down, VirtualKey key)` — keyboard
- `void OnMouseButton(bool down, int button, int x, int y)` or
  `UI::InputBlocking OnMouseButton(bool down, int button, int x, int y)` — mouse
- `void OnMouseMove(int x, int y)` — mouse move
- `void OnMouseWheel(int x, int y)` or
  `UI::InputBlocking OnMouseWheel(int x, int y)` — scroll
- `void OnLoadCallback(CMwNod@ nod)` — nod load callback

**Coroutine primitives:**
- `startnew(CoroutineFunc@ func)` — spawn a coroutine. Returns `awaitable@`.
  Has multiple overloads accepting different callback signatures (8+ variants).
- `yield()` — suspend the current coroutine until next frame.
- `sleep(uint64 ms)` — suspend the current coroutine for the specified duration.
- `awaitable` — class returned by `startnew()`. Methods: `IsRunning`, `WithRunContext`.

### 5.3 info.toml Syntax

```toml
[meta]
name = "Plugin Name"          # Recommended
version = "1.0.0"             # Required
author = "Author"             # Recommended
category = "Category"         # Recommended, defaults to "Uncategorized"
blocks = ["other-plugin-id"]  # Optional
perms = "free"                # Deprecated
siteid = 12345                # Auto-assigned by website

[game]
min_version = "2022-02-03"           # Optional, date or datetime
max_version = "2022-02-03 18:03"     # Optional

[script]
timeout = 20000                       # ms, 0 = disabled
imports = ["Dialogs.as"]              # From Openplanet's Scripts folder
exports = ["API/Export.as"]           # Compiled into dependents only
shared_exports = ["API/Shared.as"]    # Compiled into this + dependents
dependencies = ["PluginId"]           # Required dependencies
optional_dependencies = ["OptPlugin"] # Optional dependencies
export_dependencies = ["TransDep"]    # Transitive export dependencies
defines = ["MY_DEFINE"]              # Custom preprocessor defines
module = "ModuleName"                 # Override module name for exports
```

The LSP must validate all fields, types, and structural constraints.

---

## 6. Domain-Specific Semantics

**Status:** reviewed | Provenance: Confirmed (source) + Assumption (inferred)

### 6.1 Type System

**Primitive types** are built-in. All other types come from:
1. The OpenPlanet type JSON database (API types: classes, enums, functions).
2. User-defined types in `.as` source files.
3. Dependency-exported types from other plugins.

**Object handles (`T@`):**
- A nullable reference to a reference type.
- `T@ x = null;` — valid for any reference type T.
- `@x = @y;` — handle assignment (rebind, not copy).
- `x is null`, `x !is null` — null checks.
- `cast<T>(x)` — safe downcast, returns `null` on failure.
- Handle types cannot be created for value types.

**Value types vs. reference types:**
- Primitives, `vec2/3/4`, `int2/3`, `nat2/3`, `quat`, `string` are value types.
- Classes registered from the game engine are typically reference types.
- User-defined classes are reference types by default.

**Const qualification:**
- `const T` — immutable value/reference.
- `const` methods — methods that don't mutate the object.
- The LSP should track const-correctness for diagnostics.

**Templates:**
- `array<T>` — built-in generic array.
- `MwFastBuffer<T>`, `MwSArray<T>`, etc. — game-specific template types.
- `dictionary` — untyped associative container.

### 6.2 Preprocessor Evaluation Model

The preprocessor runs before parsing. It operates on lines:
1. Scan for `#if`, `#elif`, `#else`, `#endif` directives.
2. Evaluate the condition against the active define set.
3. Lines in inactive branches are excluded from parsing.
4. `&&` and `||` operators evaluate strictly left-to-right (no precedence, no parens).

**For the LSP:**
- The active define set is determined by configuration (game target + platform +
  signature mode) plus `info.toml` `[script].defines` plus dependency-derived
  `DEPENDENCY_X` defines.
- The LSP preprocesses each file before parsing, using the active define set.
- **Expert suggestion:** Consider storing both branches with their conditions, so the
  LSP can provide completion/navigation in inactive branches (with a visual indicator).
  This is optional but improves the editing experience for multi-target code.
  _Recommendation: deferred to v2._

### 6.3 Module and Namespace Resolution

**Module = plugin compilation unit.** All `.as` files under the workspace root,
plus any `imports` from `info.toml`, form a single module.

**Namespaces** are purely organizational and can span multiple files. `namespace A { }`
in file1.as and `namespace A { }` in file2.as contribute to the same namespace.

**Resolution order for symbol lookup:**
1. Local scope (block → function → class).
2. File-level globals (same file).
3. Module-level globals (all files in the compilation unit).
4. Dependency-exported symbols.
5. OpenPlanet API symbols (from type JSON database).
6. Namespace-qualified lookup: `Namespace::Symbol` skips to the named namespace.

### 6.4 Dependency Type Visibility

When plugin A depends on plugin B:
- B's `exports` files are parsed and their types are visible in A.
- B's `shared_exports` files are parsed and their types are visible in A.
- B's non-exported files are NOT visible.
- If B lists `export_dependencies = ["C"]`, then C's exports are also transitively
  visible in A.
- For `optional_dependencies`: if the dependency is found, its exports are loaded and
  `DEPENDENCY_B` is defined. If not found, no error — `DEPENDENCY_B` is simply not defined.

### 6.5 Attribute Semantics

**`[Setting ...]`** on a global variable:
- The variable's type must be one of the supported setting types.
- The LSP should validate that attribute arguments match the type (e.g., `min`/`max`
  only valid on numeric types; `color` only on `vec3`/`vec4`; `multiline`/`password`
  only on `string`).
- `if` and `enableif` reference global variables or functions — the LSP should resolve these.

**`[SettingsTab ...]`** on a global function:
- The function must have signature `void Name()`.
- `name`, `icon`, `order` attributes are optional.

**`[Persistent ...]`** on a global variable:
- Same type constraints as `[Setting]` but no UI rendering.

### 6.6 Callback Signature Validation

The LSP should validate that plugin lifecycle callbacks match their expected signatures.
Some callbacks have multiple valid signatures (e.g., `OnKeyPress` can return `void` or
`UI::InputBlocking`). The LSP should accept all documented variants and flag mismatches.

---

## 7. Functional Requirements

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs)

### 7.1 Parsing

- **FR-01:** Parse the full AngelScript syntax as used by OpenPlanet (see Section 5).
- **FR-02:** Handle incomplete/in-progress syntax gracefully — the parser must not crash
  or hang on partial input. Recovery should produce a partial AST for the valid portions.
- **FR-03:** Preprocess files before parsing, evaluating `#if`/`#elif`/`#else`/`#endif`
  against the active define set.
- **FR-04:** Parse `info.toml` and validate its schema (see Section 5.3).
- **FR-05:** Produce concrete syntax trees that preserve source positions (byte offsets
  and line/column) for all nodes, enabling accurate range mapping for LSP responses.

### 7.2 Type Resolution

- **FR-06:** Resolve types from three sources: type JSON database, user source files,
  and dependency exports. Merge them into a unified symbol table.
- **FR-07:** Resolve object handle types (`T@`), including `cast<T>()` result types.
- **FR-08:** Resolve template instantiations (`array<T>`, `MwFastBuffer<T>`, etc.).
- **FR-09:** Resolve namespace-qualified symbols (`Namespace::Symbol`).
- **FR-10:** Resolve inherited members (class inheritance chains from both source and
  API types).
- **FR-11:** Resolve `auto` types where the initializer expression type is deterministic.

### 7.3 Diagnostics

- **FR-12:** Report undefined symbols (variables, functions, types, namespaces).
- **FR-13:** Report type mismatches in assignments, function arguments, return statements.
- **FR-14:** Report invalid attribute usage (wrong type for `[Setting]`, wrong signature
  for `[SettingsTab]`, etc.).
- **FR-15:** Report callback signature mismatches for known lifecycle callbacks.
- **FR-16:** Report `info.toml` errors (malformed TOML, invalid fields, missing required
  fields, unresolvable dependencies).
- **FR-17:** Report preprocessor errors (unmatched `#if`/`#endif`, malformed conditions).
- **FR-18:** Every diagnostic against test fixtures must be either a true positive or a
  documented known issue with a tracking item.

### 7.4 Completion

- **FR-19:** Complete namespace members after `::`.
- **FR-20:** Complete class/object members after `.` (methods, properties, fields).
- **FR-21:** Complete types after `@` in handle declarations.
- **FR-22:** Complete global symbols, types, and keywords at top level.
- **FR-23:** Complete `[Setting ...]` attribute keys and values.
- **FR-24:** Complete lifecycle callback function names and signatures.
- **FR-25:** Complete `info.toml` keys and known values.
- **FR-26:** Complete `#if` define names.

### 7.5 Navigation

- **FR-27:** Go-to-definition for user-defined symbols (functions, classes, variables,
  enums, namespaces, funcdef types).
- **FR-28:** Go-to-definition for API symbols — navigate to a synthetic declaration
  or show documentation.
- **FR-29:** Find all references across the workspace.
- **FR-30:** Document outline (symbols in current file).
- **FR-31:** Workspace symbol search.

### 7.6 Hover

- **FR-32:** Show type signature on hover for any symbol.
- **FR-33:** Show documentation from type JSON database `desc` fields.
- **FR-34:** Show documentation from source code comments (preceding `//` or `/* */`).

### 7.7 Other

- **FR-35:** Signature help (parameter hints) for function/method calls.
- **FR-36:** Rename symbol across workspace (with validation).
- **FR-37:** Semantic token highlighting (types, namespaces, functions, variables,
  keywords, preprocessor directives, attributes).

---

## 8. Data and State

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs) + Assumption (inferred)

### 8.1 Type Database (from JSON)

Loaded at startup from OpenPlanet JSON files. **Two distinct JSON formats exist:**

**Format A — Core API (`OpenplanetCore.json`):**
Top-level arrays of functions, classes, and enums.
```
{
  "functions": [{ ns, name, returntypedecl, args[{typedecl, name}], decl, desc, const, ... }],
  "classes": [{ id, ns, name, desc, inherits, behaviors[], methods[], props[], ... }],
  "enums": [{ id, ns, name, desc, values: {name: int, ...} }]
}
```

**Format B — Game-specific / Nadeo types (`OpenplanetNext.json` etc.):**
Nested namespace → type → member structure with compact keys. Has a v1/v2 distinction
keyed by the presence of the `op` field.
```
{
  "op": "<version>",          // v2 marker — absent in v1
  "ns": {
    "<namespace>": {
      "<typename>": {
        "p": "<parent>",      // parent class
        "m": [                // members
          { "n": "<name>", "t": "<type>", ... }
        ]
      }
    }
  }
}
```

Both formats must be parsed and merged into a unified in-memory type database. The
conversion logic is documented in the existing extension's `convert_nadeo.ts` and
`database.ts` (`AddTypesFromOpenplanet` for Format A, `AddNadeoTypesFromOpenplanet`
for Format B). These are reference for the schema, not for architecture.

Immutable, read-only after loading. Indexed by:
- Fully qualified name (namespace + name)
- Type ID (numeric, for cross-references within Format A)

### 8.2 Symbol Table (from source)

Built incrementally from parsing `.as` files. Per-file entries, merged into a workspace-wide
table. Contains:
- **Types:** classes, interfaces, enums, funcdef types, with members.
- **Functions:** global functions with signatures.
- **Variables:** global variables (including `[Setting]` and `[Persistent]` annotated ones).
- **Namespaces:** namespace → members mapping.

Keyed by: fully qualified name, source location (file + range).

Must support:
- Per-file invalidation (on file change, drop and rebuild that file's entries).
- Cross-file dependency tracking (file A uses type from file B → if B changes, A needs re-check).

### 8.3 Dependency Cache

For each resolved dependency plugin:
- Parsed `info.toml` metadata.
- Parsed export files (AST + symbol entries).
- Cached until the dependency changes on disk (file watcher or manual reload).

### 8.4 Preprocessor State

Per-file:
- Active define set (workspace-wide defaults + info.toml defines + dependency defines).
- Per-line active/inactive status (from `#if` evaluation).
- Stored alongside the parse result for incremental updates.

### 8.5 Document State

Per open document:
- Current text content (from LSP didOpen/didChange).
- Latest parse result (AST, diagnostics, symbol contributions).
- Dirty flag for incremental re-parse.

---

## 9. Interfaces and Integrations

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs)

### 9.1 LSP Protocol

- Transport: **stdio** (stdin/stdout, JSON-RPC 2.0).
- Protocol version: LSP 3.17+ (current stable).
- Capabilities advertised at initialization (see Section 4.3 for the feature table).

### 9.2 OpenPlanet Type JSON Files

- Read at startup, reloaded on configuration change or manual reload command.
- Files: `OpenplanetCore.json` (core API), game-specific file (e.g., `OpenplanetNext.json`).
- Format: see Section 8.1.
- Located via layered configuration (see Section 4.4).

### 9.3 info.toml

- Read on workspace initialization.
- Watched for changes; re-parsed on change.
- Diagnostics published to the editor as `file:///path/to/info.toml` diagnostics.

### 9.4 Plugin Dependencies on Disk

- Located in the configured plugins directory.
- May be directories (with their own `info.toml`) or `.op` archive files.
- `.op` files: ZIP archives containing plugin source and `info.toml`.
  The LSP must extract or read from the archive to find exports.
- File watcher or manual reload for dependency changes.

### 9.5 File System

- Read `.as` source files from the workspace.
- Read config files (`.openplanet-lsp.toml`).
- Read type JSON files.
- Read dependency plugin files.
- No writes to the file system.

### 9.6 Rust Crate Dependencies (anticipated)

- `tower-lsp` or `lsp-server` — LSP protocol handling.
- `serde` + `serde_json` — JSON parsing for type DB and LSP messages.
- `toml` — TOML parsing for `info.toml` and config files.
- `logos` or custom lexer — fast tokenization.
- Parser: likely a hand-written recursive descent parser (for speed and error recovery)
  or a parser generator with good error recovery (e.g., `tree-sitter` with custom grammar,
  or `chumsky`/`winnow`).
- `dashmap` or similar — concurrent data structures for the symbol table.

**Expert suggestion:** A hand-written recursive descent parser will give the best
control over error recovery and incremental parsing, which are critical for LSP
responsiveness. Tree-sitter is an alternative that provides incremental parsing
out of the box, but writing a correct Tree-sitter grammar for AngelScript + OpenPlanet
extensions is nontrivial. _Recommendation: consider both; decide during implementation._

---

## 10. UX / Operator Experience

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs)

### 10.1 Startup

- The LSP binary is started by the editor with stdio transport.
- On startup: load configuration → load type DB → discover workspace → parse all files.
- **Target:** Startup indexing of a typical plugin (10-50 files) should complete in
  under 1 second on modern hardware.

### 10.2 Interactive Responsiveness

- **Completion:** < 100ms latency from keystroke to completion list.
- **Hover:** < 50ms.
- **Diagnostics:** Published incrementally as files are parsed; full workspace diagnostics
  within 500ms of a file save.
- **Go-to-definition / references:** < 200ms.

These are targets, not hard requirements. The design should make these achievable.

### 10.3 Error Experience

- Diagnostics should have clear, actionable messages.
- info.toml diagnostics should point to the exact key/line with the error.
- Preprocessor errors should indicate which `#if`/`#endif` is unmatched.
- Type errors should name both the expected and actual types.

### 10.4 Degraded Mode

If no `info.toml` is found:
- Operate on the single open file.
- No dependency resolution.
- Type DB loaded if configured; otherwise, only built-in primitives available.
- Diagnostic: warn that no `info.toml` was found.

If type JSON files are not found:
- Parse and provide structural analysis (syntax, navigation, symbols).
- No API type resolution — completion and diagnostics limited to user-defined types.
- Diagnostic: warn that type DB is not configured.

### 10.5 Commands

Custom LSP commands (invokable from editor command palette):
- **Reload type database** — re-read JSON files.
- **Reload dependencies** — re-resolve and re-parse dependencies.
- **Set active defines** — override the active preprocessor define set.
- **Show active defines** — display the current define set (for debugging).

---

## 11. Constraints

**Status:** reviewed | Provenance: Decision (drafted from confirmed inputs)

- **C-LANG:** Must be implemented in Rust.
- **C-PERF:** Must be fast, low memory, low CPU. Design for O(changed files) incremental
  updates, not O(all files).
- **C-PROTO:** Must use LSP protocol over stdio. No editor-specific APIs.
- **C-COMPAT:** Must handle OpenPlanet's AngelScript dialect — not standard AngelScript,
  not Unreal AngelScript. Differences must be identified and handled.
- **C-PARSE:** Parser must handle incomplete/malformed input without crashing or hanging.
  Error recovery is mandatory for an interactive LSP.
- **C-TEST:** All diagnostics emitted against real plugin fixtures must be accounted for.
  No unexamined false positives.

---

## 12. Edge Cases and Failure Modes

**Status:** reviewed | Provenance: Assumption (inferred) + Confirmed (source)

### 12.1 Parsing Edge Cases

- **Incomplete syntax during typing:** User is mid-expression. Parser must recover and
  provide partial results (completion, hover on already-parsed symbols).
- **Nested preprocessor conditionals:** `#if A && B` inside `#if C`. Must track nesting
  correctly.
- **Preprocessor in middle of declaration:** A `#if` block inside a class body or function
  that conditionally includes members or statements.
- **Multiple definitions:** Same symbol defined in multiple files (redefinition error).
- **Circular dependencies:** Plugin A depends on B depends on A. Must detect and report.
- **Very large files:** Some plugins have files > 5000 lines. Parser must not degrade
  significantly.

### 12.2 Type Resolution Edge Cases

- **Forward references:** Type B used before its declaration in the same module.
  AngelScript allows this; the LSP must handle it.
- **Template instantiation with complex types:** `array<array<CGameCtnBlock@>>`.
- **Implicit conversions:** AngelScript has implicit numeric conversions (int → float, etc.).
  The LSP type checker must handle these to avoid false positives.
- **Operator overloading:** Classes can override `opEquals`, `opCmp`, etc.
  Type checking of operators must respect overloads.
- **Property accessors:** `get_X()` / `set_X()` accessed as `obj.X`.
  Completion and type resolution must present these as properties, not methods.

### 12.3 Dependency Edge Cases

- **Missing dependency:** Required dependency not found on disk.
  Emit diagnostic on `info.toml`; continue without that dependency's types.
- **Dependency has parse errors:** Best-effort: load whatever symbols could be parsed.
- **`.op` file corruption:** Report error, skip dependency.
- **Dependency export references non-existent file:** Report error on info.toml.
- **Transitive export dependency chain:** A → B → C via `export_dependencies`.
  Must follow the chain without infinite loops.

### 12.4 Configuration Edge Cases

- **No OpenPlanet installation found:** Operate in degraded mode (see Section 10.4).
- **Type JSON file invalid/corrupt:** Report startup error, operate without type DB.
- **Config file and init params conflict:** Init params win (highest priority).

---

## 13. Acceptance Criteria

**Status:** reviewed | Provenance: Confirmed (user) + Decision (drafted from confirmed inputs)

### 13.1 Fixture Test Suite

**AC-01:** A fixture test suite is built from real TM plugin source trees selected from
`~/src/openplanet/`. The suite must include:
- At least 5 plugins of varying complexity (simple 1-3 file plugins, medium 10-20 file
  plugins, and complex 30+ file plugins).
- Plugins that use: `[Setting]` attributes, preprocessor conditionals, plugin
  dependencies, coroutines, namespaces, class inheritance, funcdef types,
  all major API namespaces (`UI::`, `Net::`, `Math::`, `nvg::`, etc.).

**AC-02:** For each fixture plugin, the LSP must produce a diagnostic report. Every
diagnostic must be categorized:
- **True positive:** A real error that OpenPlanet's compiler would also flag.
- **Known limitation:** A false positive or false negative with a documented tracking
  item explaining why and when it will be fixed.
- **Zero unexamined diagnostics.** Any new diagnostic introduced by code changes must
  be reviewed before merging.

**AC-03:** The fixture test suite runs in CI. A diff in diagnostics (new or removed)
fails the build until the diff is reviewed and the expectation file is updated.

### 13.2 Feature Acceptance

**AC-04:** Completion works for:
- Namespace members after `::` (e.g., `UI::` → `Begin`, `End`, `Text`, ...)
- Object members after `.` (e.g., `app.` → members of `CTrackMania`)
- Global symbols at top level
- `[Setting ...]` attribute keys

**AC-05:** Go-to-definition works for user-defined functions, classes, variables, and
enum values across files in the same plugin.

**AC-06:** Hover shows type signature and documentation for both API and user symbols.

**AC-07:** Diagnostics correctly identify: undefined symbols, type mismatches,
invalid `info.toml` fields, unmatched preprocessor directives.

**AC-08:** Startup time for a 50-file plugin is under 2 seconds.

**AC-09:** Incremental re-parse on single file change is under 200ms.

### 13.3 info.toml Acceptance

**AC-10:** info.toml diagnostics correctly flag:
- Malformed TOML syntax
- Unknown keys in `[meta]`, `[game]`, `[script]`
- Wrong types for known keys
- Missing `[meta].version`
- References to non-existent export files

---

## 14. Assumptions / Deferred / Unresolved

### Assumptions (inferred, not yet confirmed)

- A-01: The LSP follows the Language Server Protocol specification (JSON-RPC over stdio).
- A-02: `info.toml` presence defines the project root for workspace detection.
- A-03: All `.as` files under the project root are part of the compilation unit (unless excluded by config).
- A-04: The primary game target is TMNEXT (TrackMania 2020).
- A-05: `startnew()`, `yield()`, and `sleep()` are OpenPlanet-specific coroutine primitives (not standard AngelScript).
- A-06: The OpenPlanet type JSON files are provided by an OpenPlanet installation, not bundled with the LSP.
- A-07: The type JSON schema is `{ op, functions[], classes[], enums[] }` with namespaces, type IDs, args, descriptions. (Confirmed from source: existing extension parses this format.)
- A-08: Dependency plugins are located in the OpenPlanet installation's Plugins directory (directories and `.op` archive files).
- A-09: ~~RESOLVED by D-008/D-009~~ Layered config: auto-detect → config file → init params.

### Confirmed

- C-01: Test fixtures must use real TM plugin source files and real OpenPlanet API docs. (Confirmed (user))
- C-02: All LSP errors against fixtures must be accounted for — no silent misses. (Confirmed (user))
- C-03: OpenPlanet attribute syntax `[Setting ...]`, `[Persistent ...]`, `[SettingsTab ...]` exists in real plugins. (Confirmed (source))
- C-04: Plugin lifecycle callbacks: `Main()`, `Render()`, `RenderInterface()`, `RenderMenu()`, `RenderEarly()`, `Update(float dt)`, `OnKeyPress()`, `OnMouseButton()`, `OnMouseMove()`, `OnMouseWheel()`, `OnEnabled()`, `OnDisabled()`, `OnDestroyed()`, `OnSettingsChanged()`, `OnSettingsSave()`, `OnSettingsLoad()`, `OnLoadCallback(CMwNod@)`. (Confirmed (source))
- C-05: The `funcdef` keyword is used for first-class function type declarations. (Confirmed (source))
- C-06: `#if`/`#elif`/`#else`/`#endif` preprocessor with known defines (TMNEXT, DEV, MANIA64, etc.) (Confirmed (source))
- C-07: Implementation language is Rust. (Confirmed (user))
- C-08: Fresh build, not a fork. (Confirmed (user))
- C-09: Generic stdio LSP transport. (Confirmed (user))
- C-10: Primary target TMNEXT; game target swappable via JSON type DB. (Confirmed (user))
- C-11: Web Services API out of scope. (Confirmed (user))
- C-12: info.toml parsed with diagnostic feedback. (Confirmed (user))
- C-13: Type info JSON format is the same across all game targets. (Confirmed (user))
- C-14: Preprocessor `#if` is the primary directive used in practice. (Confirmed (user))

### Deferred

- D-01: ManiaPlanet game variants beyond TMNEXT — supported by design (swappable JSON), but not primary test target.
- D-02: Plugin signature/security mode enforcement in LSP.
- D-03: Code formatting.
- D-04: Debugger/DAP integration.
- D-05: Inactive preprocessor branch analysis (completion/navigation in `#else` branches).
- D-06: Lambda/inline function syntax support (unclear if OpenPlanet supports this).

---

## 15. Provenance and Evidence

**Status:** reviewed

See `.7/spec-drafting/provenance-log.md` (archived to `specs/archive/spec-drafting/`) for the full provenance table.

**Summary of evidence basis:**
- **858 scraped OpenPlanet doc files** provided API surface, callback signatures,
  preprocessor define lists, info.toml schema, settings attribute syntax.
- **319 scraped AngelScript docs** provided base language syntax and semantics.
- **154 real plugin source trees** provided ecosystem patterns, file structures,
  common constructs, dependency usage, and fixture candidates.
- **Existing VS Code LSP audit** provided the type JSON schema format and dependency
  resolution approach. Architectural decisions are NOT inherited.
- **User interview (this conversation)** confirmed: Rust, fresh build, stdio,
  TMNEXT primary, fixture testing, info.toml diagnostics, layered config,
  all-permissive default defines.

---

## 16. Risks

**Status:** reviewed | Provenance: Assumption (inferred)

| Risk | Impact | Likelihood | Mitigation |
|------|--------|-----------|------------|
| AngelScript grammar is large and under-documented; OpenPlanet extensions add complexity. Parser may take significant effort. | High | High | Start with a subset grammar covering the most common patterns from the 154-plugin survey. Expand iteratively. |
| Type JSON schema may evolve across OpenPlanet versions without notice. | Medium | Medium | Version-check the `op` field. Log warnings for unknown fields rather than failing. |
| Real plugins may use undocumented language features or compiler quirks. | Medium | High | Fixture test suite will surface these. Document as known limitations until addressed. |
| Dependency resolution requires reading `.op` ZIP archives, adding I/O complexity. | Low | High | Use a Rust ZIP library (`zip` crate). Cache extracted metadata. |
| All-permissive default defines may cause false positives where game-specific code branches conflict. | Medium | Medium | Document the tradeoff. Users can narrow defines via config for specific targets. |
| Incremental parsing complexity — cross-file invalidation requires dependency graph tracking. | Medium | Medium | Start with full re-parse on change; optimize to incremental once the baseline is correct. |
