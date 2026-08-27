#!/usr/bin/env node
// Locate the platform binary npm already installed, and exec it.
//
// There is deliberately NO postinstall download here. `--ignore-scripts` is
// standard policy in many corporate npm setups, enterprises consume npm
// through a mirror that a GitHub fetch would bypass, a downloaded file is
// outside npm's integrity hashes, and "downloads a binary at install time"
// is the exact supply-chain shape a security review flags. The binaries are
// `optionalDependencies`, so npm resolves exactly one of them and it is
// covered by the registry's integrity and by any mirror.
"use strict";

const { spawnSync } = require("node:child_process");

// The platform table lives in `platform.js` so it can be unit-tested; this
// file is the thin exec wrapper around it.
const { PACKAGES, packageFor, binaryName } = require("./platform.js");

const key = `${process.platform}-${process.arch}`;
const pkg = packageFor(process.platform, process.arch);

if (!pkg) {
  process.stderr.write(
    `doiget: no prebuilt binary for ${key}.\n` +
      `Supported: ${Object.keys(PACKAGES).join(", ")}.\n` +
      `Install from source instead: cargo install doiget-cli\n` +
      `(needs a C/C++ linker — see https://github.com/QAtlasHub/doiget#installation)\n`,
  );
  process.exit(2);
}

let binary;
try {
  binary = require.resolve(`${pkg}/bin/${binaryName(process.platform)}`);
} catch {
  process.stderr.write(
    `doiget: the platform package \`${pkg}\` is not installed.\n` +
      `It is an optionalDependency, so npm skips it silently when installation fails.\n` +
      `Try: npm install --include=optional ${pkg}\n`,
  );
  process.exit(2);
}

// `stdio: "inherit"` is load-bearing: `doiget serve` speaks MCP over stdio,
// so the JSON-RPC stream must be the parent's, not a pipe this shim relays.
const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  process.stderr.write(`doiget: could not execute ${binary}: ${result.error.message}\n`);
  process.exit(1);
}
// Preserve the signal-death convention so `timeout`/CI see what they expect.
process.exit(result.status === null ? 1 : result.status);
