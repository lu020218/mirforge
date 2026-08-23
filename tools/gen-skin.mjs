// MirForge UI 皮肤生成器 — 方向 A「鎏金经典」(设计定稿 2026-07-22)
// 程序化产出 Bevy UI 的 9-slice 贴图集 + 切片清单 client/assets/ui/skin/skin.json。
// 绘制: 圆角矩形 SDF + 4x 超采样抗锯齿; 改令牌重跑即可再生全套。
// 用法: node tools/gen-skin.mjs
import fs from "fs";
import path from "path";
import zlib from "zlib";
import { fileURLToPath } from "url";

const OUT = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "client", "assets", "ui", "skin");
fs.mkdirSync(OUT, { recursive: true });

// ── 设计令牌 (与设计稿 Main 画板一致) ──
const T = {
  bgDeep: [10, 10, 18], panel: [13, 15, 24], panelSolid: [18, 20, 31],
  edgeGold: [107, 90, 50], gold: [201, 165, 92], goldBright: [255, 216, 118],
  textMain: [232, 226, 208], textDim: [154, 145, 124], disabled: [91, 86, 72],
  edgeDark: [42, 45, 60], slotBg: [14, 16, 23],
  btnTop: [74, 60, 34], btnBot: [36, 29, 16],
  hp: [[224, 64, 64], [122, 20, 32]], mp: [[63, 131, 232], [22, 51, 122]],
  exp: [[122, 93, 20], [238, 205, 82]],
};

// ── PNG 编码 ──
const CRC = (() => { const t = []; for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; } return t; })();
const crc32 = (b) => { let c = 0xffffffff; for (const x of b) c = CRC[(c ^ x) & 0xff] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
function chunk(type, data) {
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type), data]);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(body));
  return Buffer.concat([len, body, crc]);
}
function encodePNG(w, h, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; ihdr[9] = 6;
  const raw = Buffer.alloc((w * 4 + 1) * h);
  for (let y = 0; y < h; y++) rgba.copy(raw, y * (w * 4 + 1) + 1, y * w * 4, (y + 1) * w * 4);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(raw, { level: 9 })), chunk("IEND", Buffer.alloc(0)),
  ]);
}

// ── SDF 画布 (4x 超采样) ──
const SS = 4;
class Canvas {
  constructor(w, h) { this.w = w; this.h = h; this.W = w * SS; this.H = h * SS; this.buf = new Float64Array(this.W * this.H * 4); }
  /** 叠加一层: shape(x,y)→sdf(<=0 在内, px 单位), color(x,y)→[r,g,b,a] (0-255/0-1) */
  layer(shape, color) {
    for (let py = 0; py < this.H; py++) for (let px = 0; px < this.W; px++) {
      const x = (px + 0.5) / SS, y = (py + 0.5) / SS;
      const d = shape(x, y);
      if (d > 0.6) continue;
      const cov = Math.min(1, Math.max(0, 0.5 - d)); // ~1px 羽化
      const [r, g, b, a] = color(x, y);
      const al = (a ?? 1) * cov;
      if (al <= 0) continue;
      const i = (py * this.W + px) * 4;
      const ba = this.buf[i + 3];
      const oa = al + ba * (1 - al);
      if (oa <= 0) continue;
      this.buf[i] = (r * al + this.buf[i] * ba * (1 - al)) / oa;
      this.buf[i + 1] = (g * al + this.buf[i + 1] * ba * (1 - al)) / oa;
      this.buf[i + 2] = (b * al + this.buf[i + 2] * ba * (1 - al)) / oa;
      this.buf[i + 3] = oa;
    }
  }
  save(name) {
    const out = Buffer.alloc(this.w * this.h * 4);
    for (let y = 0; y < this.h; y++) for (let x = 0; x < this.w; x++) {
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = 0; sy < SS; sy++) for (let sx = 0; sx < SS; sx++) {
        const i = ((y * SS + sy) * this.W + x * SS + sx) * 4;
        const al = this.buf[i + 3];
        r += this.buf[i] * al; g += this.buf[i + 1] * al; b += this.buf[i + 2] * al; a += al;
      }
      const o = (y * this.w + x) * 4;
      if (a > 0) { out[o] = r / a; out[o + 1] = g / a; out[o + 2] = b / a; }
      out[o + 3] = Math.round((a / (SS * SS)) * 255);
    }
    fs.writeFileSync(path.join(OUT, name), encodePNG(this.w, this.h, out));
  }
}
/** 圆角矩形 SDF */
const rrect = (x0, y0, x1, y1, r) => (x, y) => {
  const cx = Math.max(x0 + r - x, 0, x - (x1 - r));
  const cy = Math.max(y0 + r - y, 0, y - (y1 - r));
  return Math.hypot(cx, cy) - r;
};
const ring = (sdf, w) => (x, y) => Math.abs(sdf(x, y)) - w / 2; // 描边
const solid = (c, a = 1) => () => [c[0], c[1], c[2], a];
const vgrad = (c1, c2, y0, y1, a = 1) => (x, y) => {
  const t = Math.min(1, Math.max(0, (y - y0) / (y1 - y0)));
  return [c1[0] + (c2[0] - c1[0]) * t, c1[1] + (c2[1] - c1[1]) * t, c1[2] + (c2[2] - c1[2]) * t, a];
};
const hgrad = (c1, c2, x0, x1, a = 1) => (x) => {
  const t = Math.min(1, Math.max(0, (x - x0) / (x1 - x0)));
  return [c1[0] + (c2[0] - c1[0]) * t, c1[1] + (c2[1] - c1[1]) * t, c1[2] + (c2[2] - c1[2]) * t, a];
};

const manifest = { note: "MirForge 方向A鎏金经典皮肤 (tools/gen-skin.mjs 生成)", assets: {}, colors: {
  gold: "#c9a55c", goldBright: "#ffd876", edgeGold: "#6b5a32", textMain: "#e8e2d0", textDim: "#9a917c",
  disabled: "#5b5648", edgeDark: "#2a2d3c", panelBg: "#0d0f18", slotBg: "#0e1017",
  quality: { common: "#cfc8b4", fine: "#7bd88f", rare: "#5c9ce8", epic: "#9d7bd8", legend: "#eecd52" },
} };
const reg = (name, file, slice) => { manifest.assets[name] = { file, slice }; };

// ── panel_ornate: 四角鎏金饰边主面板 (96×96, 切片 32) ──
{
  const c = new Canvas(96, 96);
  c.layer(rrect(1, 1, 95, 95, 2), solid(T.panel, 0.94));
  c.layer(ring(rrect(1, 1, 95, 95, 2), 1), solid(T.edgeGold));
  // 四角饰边 26×26, 2px 金线
  for (const [cx0, cy0, dx, dy] of [[0, 0, 1, 1], [96, 0, -1, 1], [0, 96, 1, -1], [96, 96, -1, -1]]) {
    const hx = (x, y) => { const lx = (x - cx0) * dx, ly = (y - cy0) * dy; // 角局部坐标
      const inH = lx >= 0 && lx <= 26 && ly >= 0 && ly <= 2 ? 0 : 1;      // 横线
      const inV = ly >= 0 && ly <= 26 && lx >= 0 && lx <= 2 ? 0 : 1;      // 竖线
      return Math.min(inH, inV) === 0 ? -1 : 1; };
    c.layer(hx, solid(T.gold));
  }
  c.save("panel_ornate.png");
  reg("panelOrnate", "panel_ornate.png", [32, 32, 32, 32]);
}
// ── panel_plain / panel_glass ──
{
  const c = new Canvas(32, 32);
  c.layer(rrect(1, 1, 31, 31, 3), solid(T.panel, 0.88));
  c.layer(ring(rrect(1, 1, 31, 31, 3), 1), solid(T.edgeGold));
  c.save("panel_plain.png");
  reg("panelPlain", "panel_plain.png", [8, 8, 8, 8]);
}
{
  const c = new Canvas(32, 32);
  c.layer(rrect(1, 1, 31, 31, 4), solid([10, 11, 18], 0.55));
  c.layer(ring(rrect(1, 1, 31, 31, 4), 1), solid(T.edgeGold, 0.35));
  c.save("panel_glass.png");
  reg("panelGlass", "panel_glass.png", [8, 8, 8, 8]);
}
// ── titlebar: 面板标题条 (顶部金晕 + 底部分隔线) ──
{
  const c = new Canvas(48, 48);
  c.layer(rrect(0, 0, 48, 48, 0), vgrad([201, 165, 92], [201, 165, 92], 0, 48, 0)); // 占位透明
  c.layer(rrect(0, 0, 48, 46, 0), (x, y) => [201, 165, 92, 0.08 * (1 - y / 46)]);
  c.layer(rrect(0, 46, 48, 47, 0), solid(T.edgeDark));
  c.save("titlebar.png");
  reg("titlebar", "titlebar.png", [12, 12, 12, 12]);
}
// ── 按钮: 金 (normal/hover/pressed) + 幽灵 (normal/hover) ──
function goldBtn(name, top, bot, edge, edgeA = 1) {
  const c = new Canvas(48, 48);
  c.layer(rrect(1, 1, 47, 47, 3), vgrad(top, bot, 1, 47));
  c.layer(ring(rrect(1, 1, 47, 47, 3), 1), solid(edge, edgeA));
  c.save(name + ".png");
  reg(name.replace(/_(\w)/g, (_, ch) => ch.toUpperCase()), name + ".png", [12, 12, 12, 12]);
}
goldBtn("btn_gold", T.btnTop, T.btnBot, T.edgeGold);
goldBtn("btn_gold_hover", [90, 74, 42], [44, 36, 20], T.gold);
goldBtn("btn_gold_pressed", [30, 24, 13], [52, 42, 24], T.edgeGold);
{
  const c = new Canvas(32, 32);
  c.layer(ring(rrect(1, 1, 31, 31, 3), 1), solid(T.edgeDark));
  c.save("btn_ghost.png");
  reg("btnGhost", "btn_ghost.png", [8, 8, 8, 8]);
}
{
  const c = new Canvas(32, 32);
  c.layer(rrect(1, 1, 31, 31, 3), solid(T.gold, 0.05));
  c.layer(ring(rrect(1, 1, 31, 31, 3), 1), solid(T.edgeGold));
  c.save("btn_ghost_hover.png");
  reg("btnGhostHover", "btn_ghost_hover.png", [8, 8, 8, 8]);
}
// ── 槽位: 空/金框/选中/品质白框(运行时染色) ──
function slot(name, edge, edgeA = 1, glow = 0) {
  const c = new Canvas(48, 48);
  if (glow > 0) c.layer(ring(rrect(2, 2, 46, 46, 4), 4), solid(T.gold, 0.18));
  c.layer(rrect(2, 2, 46, 46, 3), solid(T.slotBg, name === "slot_frame" ? 0 : 1));
  c.layer(ring(rrect(2, 2, 46, 46, 3), 1), solid(edge, edgeA));
  c.save(name + ".png");
  reg(name.replace(/_(\w)/g, (_, ch) => ch.toUpperCase()), name + ".png", [6, 6, 6, 6]);
}
slot("slot", T.edgeDark);
slot("slot_gold", T.edgeGold);
slot("slot_selected", T.gold, 1, 1);
slot("slot_frame", [255, 255, 255]); // 品质染色用白框
// ── 血蓝条框 + 填充 ──
{
  const c = new Canvas(32, 14);
  c.layer(rrect(1, 1, 31, 13, 6), solid(T.slotBg));
  c.layer(ring(rrect(1, 1, 31, 13, 6), 1), solid(T.edgeGold));
  c.save("bar_frame.png");
  reg("barFrame", "bar_frame.png", [8, 6, 8, 6]);
}
function barFill(name, c1, c2) {
  const c = new Canvas(16, 12);
  c.layer(rrect(0.5, 0.5, 15.5, 11.5, 5), vgrad(c1, c2, 0.5, 11.5));
  c.save(name + ".png");
  reg(name.replace(/_(\w)/g, (_, ch) => ch.toUpperCase()), name + ".png", [6, 5, 6, 5]);
}
barFill("bar_fill_hp", T.hp[0], T.hp[1]);
barFill("bar_fill_mp", T.mp[0], T.mp[1]);
{
  const c = new Canvas(64, 5);
  c.layer(rrect(0, 0, 64, 5, 0), hgrad(T.exp[0], T.exp[1], 0, 64));
  c.save("bar_fill_exp.png");
  reg("barFillExp", "bar_fill_exp.png", [8, 2, 8, 2]);
}
// ── 等级徽章 (胶囊) ──
{
  const c = new Canvas(28, 20);
  c.layer(rrect(1, 1, 27, 19, 9), solid(T.btnBot));
  c.layer(ring(rrect(1, 1, 27, 19, 9), 1), solid(T.gold));
  c.save("badge.png");
  reg("badge", "badge.png", [10, 9, 10, 9]);
}
// ── 头像金环 (92px 圆环 + 柔光, 整图非 9-slice) ──
{
  const c = new Canvas(96, 96);
  const circle = (r) => (x, y) => Math.hypot(x - 48, y - 48) - r;
  c.layer((x, y) => Math.abs(Math.hypot(x - 48, y - 48) - 45) - 5, (x, y) => {
    const d = Math.abs(Math.hypot(x - 48, y - 48) - 45);
    return [201, 165, 92, 0.25 * Math.max(0, 1 - d / 5)];
  });
  c.layer(ring(circle(45), 2), solid(T.gold));
  c.save("avatar_ring.png");
  reg("avatarRing", "avatar_ring.png", null);
}
// ── 单角饰边 (26×26, 客户端旋转复用) ──
{
  const c = new Canvas(26, 26);
  c.layer((x, y) => (x >= 0 && x <= 26 && y >= 0 && y <= 2) || (y >= 0 && y <= 26 && x >= 0 && x <= 2) ? -1 : 1, solid(T.gold));
  c.save("corner_ornate.png");
  reg("cornerOrnate", "corner_ornate.png", null);
}

fs.writeFileSync(path.join(OUT, "skin.json"), JSON.stringify(manifest, null, 2));
console.log(`皮肤资产 ${Object.keys(manifest.assets).length} 项 → client/assets/ui/skin/`);
