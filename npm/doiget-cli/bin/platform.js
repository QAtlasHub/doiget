// The platform → package-name table, split out so it can be tested.
//
// `doiget.js` used to hold this inline, which meant the npm packaging had
// no executable test at all — only a grep in posture-lint comparing name
// lists across three files. A grep cannot catch a wrong `require.resolve`
// path or an exit code forwarded wrongly.
"use strict";

// npm's `os`/`cpu` vocabulary. The release assets use x86_64/aarch64; the
// mapping between the two lives here, in `scripts/stage-npm.sh`, and in the
// release matrix, and posture-lint checks all three agree.
const PACKAGES = {
  "darwin-arm64": "doiget-darwin-arm64",
  "darwin-x64": "doiget-darwin-x64",
  "linux-x64": "doiget-linux-x64",
  "win32-x64": "doiget-win32-x64",
};

/** Package name for a platform/arch pair, or null when unsupported. */
function packageFor(platform, arch) {
  return PACKAGES[`${platform}-${arch}`] || null;
}

/** Binary name inside the platform package. */
function binaryName(platform) {
  return platform === "win32" ? "doiget.exe" : "doiget";
}

module.exports = { PACKAGES, packageFor, binaryName };
