#!/usr/bin/env node
"use strict";

const { spawnSync } = require("child_process");
const { resolveBinaryPath } = require("../lib/platform");

let bin;
try {
  bin = resolveBinaryPath();
} catch (err) {
  console.error(err.message || err);
  process.exit(1);
}

const result = spawnSync(bin, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: false,
});

if (result.error) {
  console.error(result.error.message || result.error);
  process.exit(1);
}

process.exit(result.status === null ? 1 : result.status);
