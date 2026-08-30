# Openplanet-injected AngelScript methods (complete surface)

Established 2026-08-30 by static analysis of the shipped `Openplanet.dll`, not
by inference from plugin behaviour. This file is the authority for the
"Openplanet injects methods the Nadeo dump never lists" class of false
positive (B059, GH #21).

## Provenance

| | |
|---|---|
| Binary | `~/.local/share/Steam/steamapps/common/Trackmania/Openplanet.dll` |
| MD5 | `c179218b3b24582130ef42b92a3585f6` |
| Size / format | 12,613,120 bytes, PE32+ x86-64, 6 sections |
| Openplanet version | `1.29.5 (next, Public, 1234ad9e)` |
| Engine dump | 2026-02-03 03:51:19 |

## Method

Openplanet registers its script surface through AngelScript's
`RegisterObjectMethod` / `RegisterGlobalFunction`, whose declaration and type
arguments are **plain C string literals**. They survive into `.rdata` verbatim,
so the registration surface is recoverable without a debugger and without
running the game.

The declarations are not reachable through a pointer table — they are loaded by
`lea reg, [rip+disp32]` at each call site. So the extraction was:

1. Index every NUL-terminated ASCII string in `.rdata`/`.data` by its VMA
   (34,344 strings).
2. Linearly scan `.text` for `lea r64, [rip+disp32]` (opcode `48|4C 8D 05|0D|…`),
   resolve each displacement against the next instruction's VMA, and keep the
   hits that land on an indexed string. This yields **45,074** code→string
   references with their code addresses.
3. Group by code address. Registration sites appear as a tight run of
   `<declaration>` → `<type name>` → `"r >= 0"` (the return-code assert).

Scripts: `scripts/re/leascan.py` (step 1–2), `scripts/re/injected_methods.py`
(step 3 and the cross-checks below).

**Why the adjacency is not the evidence.** Consecutive strings in `.rdata`
merely *suggest* a grouping; the compiler is free to pool and reorder them. The
claims below rest on the **code references**, where each declaration is paired
with the type name loaded at the same call site. `GetManialinkPages` is
referenced from four distinct addresses and `GetCurrentEntityID` from two,
matching their type lists one-for-one.

## Mechanism A — universal, added to every registered engine class

Emitted by the generic class-registration loop at `0x1801dec1e`–`0x1801e0351`
(the one that logs `Registered %d classes`). These are added to **every**
reflected engine class and appear in no dump:

| Declaration | Notes |
|---|---|
| `void MwAddRef()` | GH #21 — already handled |
| `void MwRelease()` | GH #21 — already handled |
| `void opCast(?&out)` | backs `cast<T>(x)` |
| `X@ opImplCast()` | implicit nod conversion, `X` = the class |
| `X@ f()` | factory behaviour, not a callable method |

The same loop also emits `void %s(`, `%s %s(`, `%s get_%s()` and
`void set_%s(%s)` from Nadeo's reflection data — those are the *ordinary*
members, which the dump does carry. Only the five rows above are additions.

## Mechanism B — explicit per-class injections

The entire block lives at `0x180137cd3`–`0x180137f51`. **Two methods, six
registrations, and that is the whole set:**

| Declaration | Registered on | Code addresses |
|---|---|---|
| `array<CGameManialinkPage@>@ GetManialinkPages()` | `CGameNetwork`, `CGameCtnNetwork`, `CGameManiaPlanetNetwork`, `CTrackManiaNetwork` | `0x180137cd3`, `…d43`, `…db3`, `…e23` |
| `uint GetCurrentEntityID()` | `CGamePlayer`, `CSmPlayer` | `0x180137e9d`, `…f0a` |

## Mechanism C — engine string conversions

Registered on the built-in `string`/`wstring` value types, not on nods:
`void f(const SConstString &in)` on `string`; `void f(const SConstStringInt &in)`
on `string` and `wstring`.

## Why this list is complete

Completeness is the point of this exercise, so it is argued rather than
asserted. Across all of `.text` there are exactly **20** code references to a
string that names an entry in the dump's `ns` map, and every one is accounted
for:

- **6** are the Mechanism B registrations above.
- **3** are `CMwNod` — one type registration (`0x1801ddb91`) and two in the
  Mechanism A loop's base-type handling (`0x1801dee42`, `0x1801df3e6`).
- **5** are `CSystemFidsFolder` in the `Fids` namespace implementation — type
  lookups for Openplanet's own API, not registrations.
- **3** are `Bool`, `Int8`, `Int16` — primitive descriptors that live in `ns`
  alongside real classes, in Openplanet's own type-mapping table.
- **3** are `CMwCmdFastCallStaticParam`, `CMwNod` and `CMwCmdFiber` in command
  instantiation, unrelated to script registration.

An injection onto an engine type must name that type as a C string at its
registration site, so anything outside those 20 references cannot exist.

### The one near-miss, and why the tool reports it

`bool opEquals(const MwId &in)` sits immediately before the `CMwNod` type
registration at `0x1801ddb91`, and a naive "nearest preceding declaration" rule
pairs the two. It is wrong: that `opEquals` belongs to `MwId` (registered at
`0x1801dd9a0`, with its `uint32 Value` property between them), and the `CMwNod`
string is registering the *type*, not a method on it.

The discriminator is distance. A real registration passes the declaration and
the target type to the same call, so the two `lea` instructions are 10–23 bytes
apart; this false pair spans 72. `injected_methods.py` therefore confirms only
pairs within 48 bytes and **reports anything past the cut as ambiguous rather
than discarding it**, so if a compiler or layout change ever pushes a genuine
registration past the threshold it shows up for review instead of vanishing
from the table.

## Consequences for the LSP

**1. Prefer the explicit six-entry list over "base type and descendants."**
Both give identical results on this dump — the registered set is *exactly* the
full descendant subtree in both cases, verified against the `p` parent chain in
`OpenplanetNext.json`:

```
CGamePlayer  -> CSmPlayer                                    (subtree = 2, registered = 2)
CGameNetwork -> CGameCtnNetwork -> CGameManiaPlanetNetwork
                                -> CTrackManiaNetwork        (subtree = 4, registered = 4)
```

That equality is a **coincidence of the current hierarchy, not a rule.**
Openplanet hard-codes a list. If a future game update adds a `CGamePlayer`
descendant, Openplanet will not register the method on it until Openplanet
itself is updated, but a descendants-based rule in the LSP would accept it
immediately — reintroducing exactly the false positive this fixes. Mirror the
binary: match the explicit list.

**2. Two corrections to what B059 assumed.**

- Return type is `array<CGameManialinkPage@>@`, not `CGameManialinkPage@[]&`.
  `T[]` is sugar for `array<T>`, but handle (`@`) and reference (`&`) are not
  interchangeable.
- The `GetManialinkPages` base is `CGameNetwork`, not `CGameCtnNetwork`.
  A rule written as "`CGameCtnNetwork` and descendants" misses `CGameNetwork`
  itself, leaving a false positive on `CGameNetwork@` receivers.

Both came from the old `convert_nadeo.ts`, which registered only
`CGameCtnNetwork`; the shipped DLL registers four types.

## Refreshing this after an Openplanet update

Re-run `scripts/re/injected_methods.py`. It re-derives the table from whatever
DLL is installed and prints a diff against the table above, so a changed
surface shows up as a failure rather than as silent staleness. Update the
provenance block with the new MD5 and version when it does.
