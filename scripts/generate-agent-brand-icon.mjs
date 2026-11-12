// Rasterizes apps/desktop/public/easydeploymesh-brand-mark.svg into the icon
// resource the WinPE Agent embeds for its progress window. GDI cannot draw SVG,
// so the mark's geometry is mirrored here and rendered with supersampling.
// Run `node scripts/generate-agent-brand-icon.mjs` after changing the mark.
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export const ICON_SIZES = [24, 32, 48, 64];
export const ICON_PATH = fileURLToPath(
  new URL("../crates/agent/assets/brand-mark.ico", import.meta.url),
);

const SUPERSAMPLE = 8;
const CYAN = [0x06, 0xb6, 0xd4];
const WHITE = [0xff, 0xff, 0xff];
// Mark geometry in the SVG's 512-unit viewBox.
const CIRCLE = { cx: 256, cy: 256, r: 232 };
const GROUP_SCALE = 0.76;
const STROKE_HALF_WIDTH = 12;
const ROUNDED_RECTS = [
  { x: 200, y: 88, width: 112, height: 104, radius: 18 },
  { x: 80, y: 328, width: 112, height: 104, radius: 18 },
  { x: 320, y: 328, width: 112, height: 104, radius: 18 },
];
const SEGMENTS = [
  [136, 328, 136, 276],
  [160, 252, 352, 252],
  [376, 276, 376, 328],
  [256, 252, 256, 192],
];
// Quarter arcs: center, radius, and the quadrant they occupy (sign of dx, dy).
const ARCS = [
  { cx: 160, cy: 276, r: 24, sx: -1, sy: -1 },
  { cx: 352, cy: 276, r: 24, sx: 1, sy: -1 },
];

function segmentDistance(px, py, [ax, ay, bx, by]) {
  const dx = bx - ax;
  const dy = by - ay;
  const t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)));
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

function arcDistance(px, py, { cx, cy, r, sx, sy }) {
  const dx = px - cx;
  const dy = py - cy;
  if (dx * sx >= 0 && dy * sy >= 0) {
    return Math.abs(Math.hypot(dx, dy) - r);
  }
  return Math.min(Math.hypot(dx, dy - sy * r), Math.hypot(dx - sx * r, dy));
}

function roundedRectOutlineDistance(px, py, { x, y, width, height, radius }) {
  const halfWidth = width / 2;
  const halfHeight = height / 2;
  const qx = Math.abs(px - (x + halfWidth)) - (halfWidth - radius);
  const qy = Math.abs(py - (y + halfHeight)) - (halfHeight - radius);
  const outside = Math.hypot(Math.max(qx, 0), Math.max(qy, 0));
  const inside = Math.min(Math.max(qx, qy), 0);
  return Math.abs(outside + inside - radius);
}

function sampleColor(x, y) {
  if (Math.hypot(x - CIRCLE.cx, y - CIRCLE.cy) > CIRCLE.r) {
    return null;
  }
  const gx = 256 + (x - 256) / GROUP_SCALE;
  const gy = 256 + (y - 256) / GROUP_SCALE;
  const distance = Math.min(
    ...ROUNDED_RECTS.map((rect) => roundedRectOutlineDistance(gx, gy, rect)),
    ...SEGMENTS.map((segment) => segmentDistance(gx, gy, segment)),
    ...ARCS.map((arc) => arcDistance(gx, gy, arc)),
  );
  return distance <= STROKE_HALF_WIDTH ? WHITE : CYAN;
}

/** Returns straight-alpha BGRA rows, top row first. */
export function renderBrandMark(size) {
  const pixels = Buffer.alloc(size * size * 4);
  const samples = SUPERSAMPLE * SUPERSAMPLE;
  for (let row = 0; row < size; row += 1) {
    for (let column = 0; column < size; column += 1) {
      let covered = 0;
      const sum = [0, 0, 0];
      for (let sy = 0; sy < SUPERSAMPLE; sy += 1) {
        for (let sx = 0; sx < SUPERSAMPLE; sx += 1) {
          const x = ((column + (sx + 0.5) / SUPERSAMPLE) * 512) / size;
          const y = ((row + (sy + 0.5) / SUPERSAMPLE) * 512) / size;
          const color = sampleColor(x, y);
          if (color) {
            covered += 1;
            sum[0] += color[0];
            sum[1] += color[1];
            sum[2] += color[2];
          }
        }
      }
      const offset = (row * size + column) * 4;
      if (covered > 0) {
        pixels[offset] = Math.round(sum[2] / covered);
        pixels[offset + 1] = Math.round(sum[1] / covered);
        pixels[offset + 2] = Math.round(sum[0] / covered);
        pixels[offset + 3] = Math.round((covered * 255) / samples);
      }
    }
  }
  return pixels;
}

function iconImage(size) {
  const pixels = renderBrandMark(size);
  const maskStride = Math.ceil(size / 32) * 4;
  const header = Buffer.alloc(40);
  header.writeUInt32LE(40, 0);
  header.writeInt32LE(size, 4);
  header.writeInt32LE(size * 2, 8);
  header.writeUInt16LE(1, 12);
  header.writeUInt16LE(32, 14);
  header.writeUInt32LE(size * size * 4 + maskStride * size, 20);
  const bottomUp = Buffer.alloc(size * size * 4);
  for (let row = 0; row < size; row += 1) {
    pixels.copy(bottomUp, (size - 1 - row) * size * 4, row * size * 4, (row + 1) * size * 4);
  }
  // The AND mask is ignored for 32-bit images with alpha; keep it all-opaque.
  return Buffer.concat([header, bottomUp, Buffer.alloc(maskStride * size)]);
}

export function buildBrandIcon() {
  const images = ICON_SIZES.map(iconImage);
  const directory = Buffer.alloc(6 + 16 * images.length);
  directory.writeUInt16LE(0, 0);
  directory.writeUInt16LE(1, 2);
  directory.writeUInt16LE(images.length, 4);
  let offset = directory.length;
  images.forEach((image, index) => {
    const entry = 6 + 16 * index;
    const size = ICON_SIZES[index];
    directory.writeUInt8(size, entry);
    directory.writeUInt8(size, entry + 1);
    directory.writeUInt16LE(1, entry + 4);
    directory.writeUInt16LE(32, entry + 6);
    directory.writeUInt32LE(image.length, entry + 8);
    directory.writeUInt32LE(offset, entry + 12);
    offset += image.length;
  });
  return Buffer.concat([directory, ...images]);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  writeFileSync(ICON_PATH, buildBrandIcon());
  console.log(`Wrote ${ICON_PATH}`);
}
