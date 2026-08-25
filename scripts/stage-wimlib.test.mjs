import test from "node:test";
import assert from "node:assert/strict";
import { rustTarget, WIMLIB_SOURCE_SHA256, WIMLIB_VERSION } from "./stage-wimlib.mjs";

test("wimlib source and supported targets are pinned", () => {
  assert.equal(WIMLIB_VERSION, "1.14.5");
  assert.match(WIMLIB_SOURCE_SHA256, /^[0-9a-f]{64}$/);
  assert.equal(rustTarget("macos-arm64"), "aarch64-apple-darwin");
  assert.equal(rustTarget("macos-x64"), "x86_64-apple-darwin");
  assert.throws(() => rustTarget("windows-x64"), /unsupported/);
});
