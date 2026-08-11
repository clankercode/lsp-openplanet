"use strict";

/**
 * Map Node's process.platform + process.arch to an optional platform package.
 * Keep in sync with .github/workflows/release.yml matrix `npm_pkg` values.
 */
const PLATFORMS = {
  "darwin-arm64": {
    package: "openplanet-lsp-darwin-arm64",
    binary: "openplanet-lsp",
  },
  "darwin-x64": {
    package: "openplanet-lsp-darwin-x64",
    binary: "openplanet-lsp",
  },
  "linux-arm64": {
    package: "openplanet-lsp-linux-arm64",
    binary: "openplanet-lsp",
  },
  "linux-x64": {
    package: "openplanet-lsp-linux-x64",
    binary: "openplanet-lsp",
  },
  "win32-arm64": {
    package: "openplanet-lsp-win32-arm64",
    binary: "openplanet-lsp.exe",
  },
  "win32-x64": {
    package: "openplanet-lsp-win32-x64",
    binary: "openplanet-lsp.exe",
  },
};

function platformKey(platform = process.platform, arch = process.arch) {
  return `${platform}-${arch}`;
}

function platformInfo(platform = process.platform, arch = process.arch) {
  const key = platformKey(platform, arch);
  return { key, ...(PLATFORMS[key] || {}) };
}

/**
 * Resolve the absolute path to the platform binary, or throw a helpful error.
 */
function resolveBinaryPath(opts = {}) {
  const { key, package: pkgName, binary } = platformInfo(opts.platform, opts.arch);
  if (!pkgName) {
    const supported = Object.keys(PLATFORMS).join(", ");
    throw new Error(
      `openplanet-lsp: unsupported platform '${key}'. Supported: ${supported}`
    );
  }

  let pkgRoot;
  try {
    pkgRoot = require.resolve(`${pkgName}/package.json`);
  } catch (err) {
    throw new Error(
      `openplanet-lsp: platform package '${pkgName}' is not installed.\n` +
        `This usually means optionalDependencies failed (offline install, ` +
        `registry mirror, or unsupported libc). Try:\n` +
        `  npm install ${pkgName}@${require("../package.json").version}\n` +
        `Or download a release binary from:\n` +
        `  https://github.com/clankercode/lsp-openplanet/releases`
    );
  }

  const path = require("path");
  const fs = require("fs");
  const binPath = path.join(path.dirname(pkgRoot), "bin", binary);
  if (!fs.existsSync(binPath)) {
    throw new Error(
      `openplanet-lsp: binary missing in ${pkgName}: expected ${binPath}`
    );
  }
  return binPath;
}

module.exports = {
  PLATFORMS,
  platformKey,
  platformInfo,
  resolveBinaryPath,
};
