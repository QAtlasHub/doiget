// Executable tests for the npm packaging (#511 follow-up).
//
// The packaging previously had NO test that ran anything — only a
// posture-lint grep comparing name lists across three files. A grep cannot
// catch a wrong binary name, a missing platform, or a table that agrees
// with itself while naming the wrong thing.
//
// Run: node npm/doiget-cli/test/platform.test.js
"use strict";

const assert = require("node:assert");
const fs = require("node:fs");
const path = require("node:path");

const { PACKAGES, packageFor, binaryName } = require("../bin/platform.js");

let failures = 0;
function test(name, fn) {
  try {
    fn();
    console.log(`ok    ${name}`);
  } catch (e) {
    console.error(`FAIL  ${name}\n      ${e.message}`);
    failures += 1;
  }
}

test("every supported platform resolves to a package", () => {
  assert.strictEqual(packageFor("darwin", "arm64"), "doiget-darwin-arm64");
  assert.strictEqual(packageFor("darwin", "x64"), "doiget-darwin-x64");
  assert.strictEqual(packageFor("linux", "x64"), "doiget-linux-x64");
  assert.strictEqual(packageFor("win32", "x64"), "doiget-win32-x64");
});

test("an unsupported platform is null, not undefined or a throw", () => {
  assert.strictEqual(packageFor("freebsd", "x64"), null);
  assert.strictEqual(packageFor("linux", "arm64"), null, "linux-arm64 is not built yet");
});

test("only Windows gets the .exe suffix", () => {
  assert.strictEqual(binaryName("win32"), "doiget.exe");
  assert.strictEqual(binaryName("linux"), "doiget");
  assert.strictEqual(binaryName("darwin"), "doiget");
});

test("every package in the table exists on disk with a matching manifest", () => {
  const root = path.join(__dirname, "..", "..");
  for (const pkg of Object.values(PACKAGES)) {
    const manifest = path.join(root, pkg, "package.json");
    assert.ok(fs.existsSync(manifest), `${pkg}/package.json is missing`);
    const parsed = JSON.parse(fs.readFileSync(manifest, "utf8"));
    assert.strictEqual(parsed.name, pkg, `${pkg} manifest names ${parsed.name}`);
    assert.ok(Array.isArray(parsed.os) && parsed.os.length === 1, `${pkg} needs one os`);
    assert.ok(Array.isArray(parsed.cpu) && parsed.cpu.length === 1, `${pkg} needs one cpu`);
    // The directory name encodes os-cpu; the manifest must agree with it.
    assert.strictEqual(`doiget-${parsed.os[0]}-${parsed.cpu[0]}`, pkg);
  }
});

test("the wrapper declares exactly the packages in the table", () => {
  const wrapper = JSON.parse(
    fs.readFileSync(path.join(__dirname, "..", "package.json"), "utf8"),
  );
  assert.deepStrictEqual(
    Object.keys(wrapper.optionalDependencies).sort(),
    Object.values(PACKAGES).sort(),
    "optionalDependencies and the platform table disagree",
  );
});

test("the wrapper ships the shim and the table it requires", () => {
  const dir = path.join(__dirname, "..", "bin");
  for (const f of ["doiget.js", "platform.js"]) {
    assert.ok(fs.existsSync(path.join(dir, f)), `bin/${f} is missing`);
  }
});

process.exit(failures === 0 ? 0 : 1);
