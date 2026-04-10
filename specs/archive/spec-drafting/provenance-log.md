# Provenance Log

| Claim | Label | Evidence Source | Linked Sections | Notes |
|-------|-------|-----------------|-----------------|-------|
| Plugin lifecycle callbacks list (Main, Render, etc.) | Confirmed (source) | ~/scrape/openplanet/root/docs/reference/plugin-callbacks.md | S1, S5, S7 | |
| OpenPlanet attribute syntax [Setting ...] exists | Confirmed (source) | 154 plugin survey + openplanet docs | S5, S6 | |
| Preprocessor directives #if/#elif/#else/#endif with TMNEXT etc. | Confirmed (source) | ~/scrape/openplanet/root/docs/reference/preprocessor.md + plugins | S5, S6 | |
| info.toml is project root marker | Confirmed (source) | 154 plugin survey; vscode extension uses it as root | S2, S9 | |
| ~25 API namespaces, 500+ types | Confirmed (source) | ~/scrape/openplanet/root/docs/api/ (513 files) | S1, S7 | |
| Test fixtures from real plugins, all errors accounted for | Confirmed (user) | User messages in this session | S7, S13 | |
| funcdef keyword used in real plugins | Confirmed (source) | Plugin survey (tm-editor-plus-plus, others) | S5 | |
| startnew() / yield() coroutine primitives | Confirmed (source) | Plugin survey + openplanet docs | S5, S6 | |
| Implementation language: Rust | Confirmed (user) | User: "principled decisions for rust" | S2, S9, S11 | |
| Fresh build, not fork | Confirmed (user) | User: "algorithmically bad" re existing LSP | S2, S9 | |
| Generic stdio transport | Confirmed (user) | User selected "Generic stdio LSP" | S2, S9, S10 | |
| TMNEXT primary; game target swappable via JSON | Confirmed (user) | User: "type info in those json files is same format for all games" | S2, S5 | |
| Web Services API out of scope | Confirmed (user) | User: "no native support for that in openplanet angelscript" | S2 | |
| info.toml parsed with diagnostic feedback | Confirmed (user) | User: "LSP should find and parse info.toml and provide error feedback" | S7, S9 | |
| Preprocessor #if is primary directive used | Confirmed (user) | User: "really only #IF is used" | S5, S6 | |
| Type info JSON format same across all games | Confirmed (user) | User in Q4 answer | S2, S8 | |
| Existing VS Code LSP is reference only | Confirmed (user) | User: "don't let its design influence you" | S2 | |
