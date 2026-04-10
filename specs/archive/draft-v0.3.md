# OpenPlanet LSP — Specification

**Status:** draft-v0.3 (behavior drafted)
**Stable path:** `specs/draft-v1.md`
**Archive:** `specs/archive/` | index: `.7/spec-drafting/archive-index.md`

## Source Inputs

- `~/scrape/openplanet/` — 858 scraped OpenPlanet doc files (API, reference, tutorials)
- `~/scrape/angelcode/` — 319 scraped AngelScript language docs
- `~/src/openplanet/` — 154 real TM plugin source trees (ecosystem survey)
- `~/src/openplanet/vscode-openplanet-angelscript/` — existing VS Code LSP (reference only; not forking)
- User input (this conversation)

**Source basis note:** Primary source evidence comes from scraped OpenPlanet developer docs and real plugin codebases. The existing VS Code LSP is used as a reference for what features exist, but not as an architectural or algorithmic influence. Claims about behavior are confirmed from source unless labeled otherwise.

---

## Section Status Table

| # | Section | Status | Notes |
|---|---------|--------|-------|
| 1 | Problem and goals | reviewed | Foundation confirmed Round 1 |
| 2 | Scope + non-goals | reviewed | All blocking decisions resolved |
| 3 | Users and actors | reviewed | Straightforward from scope |
| 4 | Core workflows | drafting | Round 2 |
| 5 | Concrete syntax (AngelScript/OpenPlanet) | drafting | Round 2 |
| 6 | Domain-specific semantics | drafting | Round 2 |
| 7 | Functional requirements | pending | |
| 8 | Data and state | pending | |
| 9 | Interfaces and integrations | pending | |
| 10 | UX / operator experience | pending | |
| 11 | Constraints | pending | |
| 12 | Edge cases and failure modes | pending | |
| 13 | Acceptance criteria | pending | |
| 14 | Assumptions / deferred / unresolved | drafting | Ongoing |
| 15 | Provenance and evidence | drafting | Ongoing |
| 16 | Risks | pending | |

---

## Decision Notes (summary)

- **D-001** Confirmed (user): Implementation language is **Rust**. Design priorities: fast, accurate, low memory/CPU.
- **D-002** Confirmed (user): **Fresh build**. Existing VS Code LSP is "algorithmically bad" — reference only, not a fork target.
- **D-003** Confirmed (user): **Generic stdio LSP** — editor-agnostic, works with any LSP client.
- **D-004** Confirmed (user): Real plugin source files + API docs used as test fixtures; all errors must be accounted for.
- **D-005** Confirmed (user): Primary game target **TMNEXT**. Type info JSON files share the same format across all games — LSP should support swapping game target. Preprocessor `#if` handling is the key difficulty.
- **D-006** Confirmed (user): **Web Services API is out of scope** — no native support in OpenPlanet AngelScript.
- **D-007** Confirmed (user): **info.toml must be parsed by the LSP** with error feedback (diagnostics on the TOML itself).

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

**Status:** drafting | Provenance: Decision (drafted from confirmed inputs) + Confirmed (source)

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

**UNRESOLVED (needs-user):** How does the user configure:
- Path to OpenPlanet type JSON files?
- Path to installed plugins directory (for dependency resolution)?
- Active game target / platform / signature mode defines?

Options: LSP initialization params, `.openplanet-lsp.toml` config file, or derive from
known OpenPlanet installation paths.

---

## 5. Concrete Syntax (AngelScript / OpenPlanet Dialect)

**Status:** drafting | Provenance: Confirmed (source)

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
- `string`
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
#if DEFINE1 && DEFINE2
#if DEFINE1 || DEFINE2
#elif DEFINE
#else
#endif
```
- Operators `&&` and `||` evaluate left-to-right with no precedence rules.
- Parentheses are NOT supported for grouping.
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
- `startnew(CoroutineFunc@ func)` — spawn a coroutine
- `yield()` — suspend the current coroutine

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

**Status:** drafting | Provenance: Confirmed (source) + Assumption (inferred)

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

## 7–16. [Pending — next: Functional requirements, Data and state]

---

## Assumptions / Deferred / Unresolved

### Assumptions (inferred, not yet confirmed)

- A-01: The LSP follows the Language Server Protocol specification (JSON-RPC over stdio).
- A-02: `info.toml` presence defines the project root for workspace detection.
- A-03: All `.as` files under the project root are part of the compilation unit (unless excluded by config).
- A-05: `startnew()` and `yield()` are OpenPlanet-specific coroutine primitives (not standard AngelScript).
- A-06: The OpenPlanet type JSON files are provided by an OpenPlanet installation, not bundled with the LSP.
- A-07: The type JSON schema is `{ op, functions[], classes[], enums[] }` with namespaces, type IDs, args, descriptions. (Confirmed from source: existing extension parses this format.)
- A-08: Dependency plugins are located in the OpenPlanet installation's Plugins directory (directories and `.op` archive files).
- A-09: The LSP will need a configuration mechanism to know the paths to: type JSON files, installed plugins directory, and active define set.

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
