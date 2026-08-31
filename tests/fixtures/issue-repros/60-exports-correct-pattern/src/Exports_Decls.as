// The strict `[script] exports` file: ONLY `import <sig> from "<module>";`
// declarations. It compiles into CONSUMERS and never into this plugin, so the
// bodies these bind to live in an ordinary source file (Main.as).
//
// WHAT THIS FIXTURE IS EVIDENCE OF
// --------------------------------
// That each stub below is written the way a stub HAS to be written for a
// consumer to reach it: the engine binds an import to its body by EXACT
// signature, so every stub here matches its body character for character —
// `const`, `&in`/`&out`, `@`, the default argument, all of it.
//
// MEASURED, not assumed. Game-probed against Openplanet 1.29.14 on
// 2026-08-31 by `scripts/probe_export_binding.py` — an exporter plugin
// holding every body, one consumer plugin per shape CALLING the import, with
// a positive AND a negative control. 18 variants, 0 ambiguous. The verbatim
// verdicts are committed at `docs/probes/b086-export-stub-binding.tsv`.
//
// The rule the engine actually follows:
//   * an EXACT signature match binds, in every shape tried — by value,
//     `const &in`, `&out`, `int &out`, `T@`, `T@ &in`, and a defaulted
//     parameter;
//   * ANY difference in a parameter type does not bind, and the consumer
//     dies at runtime with `Unbound function called`. `const` alone is
//     enough: a `string &in` stub over a `const string &in` body fails.
//   * the RETURN type counts too — identical parameters with `string` vs
//     `void` does not bind;
//   * a DEFAULT ARGUMENT does not: present on only one side, it still binds,
//     in both directions;
//   * the resolved type is what matters, not its spelling — a `uint` stub
//     over a `uint32` body binds.
//
// Corroborated independently by a shipped library on the probing machine:
// `ai-api`'s export stubs are byte-identical to their bodies in
// `AiApi_Net.as`, and `tm-agent` calls them at runtime.
//
// WHAT IT IS NOT EVIDENCE OF
// --------------------------
// Anything about openplanet-lsp's own matching rule. B062 matches on simple
// name and arity only, never on types, and under-approximates the engine
// deliberately — see `src/analysis/unused.rs::exported_signature_roots`.
// A stub here that did NOT match its body would still be rooted by the LSP
// and would still be broken in the game.
//
// The version of this file before B086 paired a by-value stub with the
// `const string &in` body below and called it "the CORRECT pattern". It had
// never been measured, and it was cited as evidence twice — once in a
// shipped source comment, once when approving B062's rule — before a game
// probe caught it. A fixture comment asserting engine behaviour is a claim,
// not a measurement (docs/verification-hazards.md, Mechanism 3).
import string SendEvent(const string &in a, const string &in b) from "T";

// The same rule across the other parameter shapes a real export uses. Each
// matches its body in Main.as exactly; a mismatch in any one of them is the
// same runtime `Unbound function called`, not a compile error on either side.
import void FillLast(string &out slot) from "T";        // `&out`
import string Tag(string &in raw) from "T";             // non-const `&in`
import Payload@ Wrap(Payload@ p) from "T";              // handle `T@`
import bool Accepts(Payload@ &in p) from "T";           // handle by reference
import int Tally(int a, int b = 2) from "T";            // defaulted parameter
import uint Bump(uint n) from "T";                      // `uint`/`uint32` spelling
