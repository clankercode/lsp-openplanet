// STRICT `[script] exports`: only `import <sig> from "Mod";` stubs. Compiles
// into CONSUMERS, never into this plugin. B062 reads these as a root set and
// matches them against the ordinary bodies elsewhere in the plugin.
import bool IsConnected() from "T";

// B087: the export surface is itself namespaced, and so is the body it binds
// to — the shape tm-editor-plus-plus uses throughout (`namespace Editor`
// around both the stubs and the implementations). B062 refused any body whose
// pooled name contained `::`, so 37 live exports in that plugin were reported
// unused.
namespace Surface {
    import int NsExported() from "T";
    import int DeepNsExported() from "T";
}
