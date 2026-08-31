// A STRICT export file carries only `import <sig> from "Mod";` declarations
// and compiles into CONSUMERS, never into this plugin. The bodies these bind
// to live in ordinary source files (see 60-exports-correct-pattern, which
// states the rule). Exempting THIS file — which the analysis already did —
// cannot reach those bodies, which is why the manifest had to root them.
import string SendEvent(const string &in kind, const string &in payload) from "T";
import bool IsConnected() from "T";
// Default arguments: the stub's arity RANGE must overlap the body's.
import int Tally(int a, int b = 2) from "T";
