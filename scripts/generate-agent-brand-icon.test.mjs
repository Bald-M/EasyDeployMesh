import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { buildBrandIcon, ICON_PATH, ICON_SIZES, renderBrandMark } from "./generate-agent-brand-icon.mjs";

test("committed Agent brand icon matches the generator", () => {
  assert.ok(
    readFileSync(ICON_PATH).equals(buildBrandIcon()),
    "run `node scripts/generate-agent-brand-icon.mjs` to regenerate crates/agent/assets/brand-mark.ico",
  );
});

test("brand mark is a cyan disc with a white glyph and transparent corners", () => {
  const size = 64;
  const pixels = renderBrandMark(size);
  const pixel = (x, y) => [...pixels.subarray((y * size + x) * 4, (y * size + x) * 4 + 4)];
  assert.deepEqual(pixel(0, 0), [0, 0, 0, 0]);
  assert.deepEqual(pixel(32, 6), [0xd4, 0xb6, 0x06, 0xff]);
  // Vertical connector between the top node and the bus, at x = 256 in mark space.
  assert.deepEqual(pixel(32, 27).slice(0, 3), [0xff, 0xff, 0xff]);
  assert.deepEqual(ICON_SIZES, [24, 32, 48, 64]);
});
