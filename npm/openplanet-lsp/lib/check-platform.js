"use strict";

// Soft postinstall check — optionalDependencies may legitimately be missing
// when npm installs on an unsupported host (e.g. building a container image
// for another arch). Only warn; the bin wrapper hard-fails at runtime.
const { platformInfo } = require("./platform");

const info = platformInfo();
if (!info.package) {
  console.warn(
    `[openplanet-lsp] warning: no prebuilt binary for ${info.key}. ` +
      `The 'openplanet-lsp' command will fail until you install a matching ` +
      `platform package or use a GitHub release binary.`
  );
  process.exit(0);
}

try {
  require.resolve(`${info.package}/package.json`);
} catch {
  console.warn(
    `[openplanet-lsp] warning: optional platform package '${info.package}' ` +
      `did not install. The CLI may be unavailable on this machine.`
  );
}
