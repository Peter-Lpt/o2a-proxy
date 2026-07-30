#!/usr/bin/env node
/**
 * 生成 macOS 菜单栏模板图标（黑底透明，自动适配明暗模式）。
 * 仅依赖 Node 内置 zlib，无需任何 npm 包。
 * 运行: node gen-icon.js  -> 输出 assets/tray.png
 */
const fs = require("fs");
const path = require("path");
const zlib = require("zlib");

const SIZE = 24;
const TRANSPARENT = [0, 0, 0, 0];
const BLACK = [0, 0, 0, 255];

function inTriangle(px, py, ax, ay, bx, by, cx, cy) {
  const d1 = (px - bx) * (ay - by) - (ax - bx) * (py - by);
  const d2 = (px - cx) * (by - cy) - (bx - cx) * (py - cy);
  const d3 = (px - ax) * (cy - ay) - (cx - ax) * (py - ay);
  const hasNeg = d1 < 0 || d2 < 0 || d3 < 0;
  const hasPos = d1 > 0 || d2 > 0 || d3 > 0;
  return !(hasNeg && hasPos);
}

function inRect(px, py, x0, y0, x1, y1) {
  return px >= x0 && px <= x1 && py >= y0 && py <= y1;
}

// 构建一个 24x24 RGBA 缓冲
const buf = Buffer.alloc(SIZE * SIZE * 4);
for (let i = 0; i < SIZE * SIZE; i++) {
  const o = i * 4;
  buf[o] = 0; buf[o + 1] = 0; buf[o + 2] = 0; buf[o + 3] = 0;
}

// 绘制两个相对的箭头（⇄），象征协议转换 / 双向代理
for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    let black = false;
    // 左箭头（指向左）：tip 在 (5,12)
    if (inTriangle(x, y, 5, 12, 11, 7, 11, 17)) black = true;
    // 右箭头（指向右）：tip 在 (19,12)
    if (inTriangle(x, y, 19, 12, 13, 7, 13, 17)) black = true;
    // 中间连接横线
    if (inRect(x, y, 8, 11, 16, 13)) black = true;
    if (black) {
      const o = (y * SIZE + x) * 4;
      buf[o] = BLACK[0]; buf[o + 1] = BLACK[1]; buf[o + 2] = BLACK[2]; buf[o + 3] = BLACK[3];
    }
  }
}

// ---- 最小 PNG 编码器 ----
function crc32(buf) {
  let c;
  const table = crc32.table || (crc32.table = (() => {
    const t = [];
    for (let n = 0; n < 256; n++) {
      c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      t[n] = c >>> 0;
    }
    return t;
  })());
  let crc = 0xffffffff;
  for (let i = 0; i < buf.length; i++) crc = table[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crcBuf = Buffer.alloc(4);
  crcBuf.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crcBuf]);
}

const sig = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8;  // bit depth
ihdr[9] = 6;  // color type RGBA
ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;

// 每行前加 filter byte 0
const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0;
  buf.copy(raw, y * (SIZE * 4 + 1) + 1, y * SIZE * 4, (y + 1) * SIZE * 4);
}
const idat = zlib.deflateSync(raw, { level: 9 });

const png = Buffer.concat([
  sig,
  chunk("IHDR", ihdr),
  chunk("IDAT", idat),
  chunk("IEND", Buffer.alloc(0)),
]);

const outDir = path.join(__dirname, "assets");
fs.mkdirSync(outDir, { recursive: true });
// 以 Template 结尾，macOS 菜单栏会将其作为单色模板图标（自动适配明暗）
fs.writeFileSync(path.join(outDir, "trayTemplate.png"), png);
console.log("wrote", path.join(outDir, "trayTemplate.png"), png.length, "bytes");
