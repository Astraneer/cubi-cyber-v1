// Эталонный маршрут на видеокарте ноутбука, итоги, сравнение сборок и метрики вида (Q16, команда bench; docs/areas/bench.md).
//
//   node perf/bench/route.mjs run  [--tag имя] [--root папка/web] [--presets default,heavy] [--reps 2] [--only отрезки]
//                                  [--no-look] [--scenes сцены] [--q "ключ=знач&…"] [--wait 20] [--note текст]
//   node perf/bench/route.mjs look [--tag имя] [--scenes shadows,far] [--q shsnap=0]      только метрики вида
//   node perf/bench/route.mjs compare <A> <B>     что стало лучше / хуже и где (A, B — сборка или сборка/сеанс)
//   node perf/bench/route.mjs page [A B …]        HTML-страница сравнения (по умолчанию — две последние сборки)
//   node perf/bench/route.mjs ref <сборка>        эталон для попиксельного сравнения (сравнивается с ним каждый look)
//   node perf/bench/route.mjs list | status       сборки и сеансы | замок и нагрузка машины сейчас
//
// run: замок .bench.lock (tools/check.sh и build.sh ждут) → тишина (чужой процессор, видеокарта, проверка check.sh) →
// повторы маршрута web/game/route.js (?route=all), пресеты «По умолчанию» и «Тяжелее» поочерёдно, каждый в свежем Chrome
// (холст 3440×1440, ANGLE D3D11, без vsync — кадр = его цена) → кадры для метрик вида (?look=сцена) → итог, журнал,
// страница. Всё — в BENCH_DIR (voxel-wt/_bench): <сборка>/<сеанс>/{route-<пресет>-<n>.json, look/…}, journal.jsonl,
// index.html. Шум: во время прогона чужая нагрузка пишется раз в ~4 с; выше порога — прогон «грязный» (в итог не идёт,
// если есть чистые).
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { BENCH_DIR, REPO, LOCK, sleep, buildName, takeLock, releaseLock, waitQuiet, loadSampler, foreignLoad, gpuState, serve,
  openChrome, decodePng, encodePng, frameStats, costStats } from "./lib.mjs";
import { writePage } from "./page.mjs";

const args = process.argv.slice(2);
const cmd = args[0] || "run";
const opt = (k, d) => { const i = args.indexOf(k); return i >= 0 && i + 1 < args.length ? args[i + 1] : d; };
const flag = (k) => args.includes(k);
const pos = args.slice(1).filter((a, i, all) => !a.startsWith("--") && !(i > 0 && all[i - 1].startsWith("--") && !["--no-look", "--no-route", "--vsync"].includes(all[i - 1])));

// Пресеты (web/settings.js PRESET_DEF): «По умолчанию» — умолчания графики (MSAA, тени 2 каскада, SSAO), «Тяжелее» —
// дальность 512 и «блоки до» 40. MSAA и тени — явно включены. Авто-качество и темп в ?route выключены (HANDS_FREE).
export const PRESETS = { default: { msaa: 1, shadows: 1 }, heavy: { msaa: 1, shadows: 1, view: 512, near: 40 } };
const COMMON = { nohelp: 1, telemetry: 0, dayRun: 0, holdFps: 0, autoQuality: 0 };
// Сцены метрик вида: точка, взгляд, время; v — движение камеры (блоков/с), turn — поворот (°/с). Медленное движение
// нужно теням (дрожь при сдвиге каскада) и дали (рябь LOD); листва и вода — стоя (ветер, волны).
export const SCENES = {
  // Тени: камера стоит, центр каскадов едет 0,5 блока/с (?shdrift): так тени ведут себя при ходьбе, а остальное стоит —
  // с привязкой каскадов к текселям тени неподвижны, без неё — ползут и дрожат.
  shadows: { about: "тени под деревьями: камера стоит, каскады едут 0,5 блока/с (как при ходьбе)", x: -396.5, z: 380.5, lookdy: 4, yaw: 0.6, pitch: -0.6, time: 0.42, q: "shdrift=0.5" },
  walk: { about: "та же поляна, камера плывёт вбок 0,5 блока/с (всё вместе: текстуры, трава, тени)", x: -396.5, z: 380.5, lookdy: 4, yaw: 0.6, pitch: -0.6, time: 0.42, v: [0.5, 0, 0] },
  leaves: { about: "кроны вблизи, камера стоит (ветер)", x: -400, z: 384, lookdy: 9, yaw: 0.6, pitch: -0.05, time: 0.42 },
  water: { about: "берег и море, камера стоит (волны, отражения)", x: 128, y: 72, z: -224, yaw: 0, pitch: -0.15, time: 0.4 },
  far: { about: "даль с вершины, поворот 1°/с (рябь дальнего LOD)", x: -416, y: 140, z: 992, yaw: 2.4, pitch: -0.12, time: 0.45, turn: 1 },
  crossroads: { about: "витрина машин на Перекрёстке, камера стоит", x: -5.5, y: 73.6, z: 3.5, yaw: -1.45, pitch: -0.12, time: 0.42 },
  night: { about: "Перекрёсток ночью: Маяк и свечение машин", x: 0.5, z: 40.5, lookdy: 1.7, yaw: 0, pitch: 0.3, time: 0.0 },
};
const LOOK_N = 60; // кадров на сцену (1 с при 60 к/с виртуального времени)
const DIFF_T = 16; // попиксельное сравнение: пиксель изменился, если канал разошёлся больше чем на 16 из 255

// ---------------------------------------------------------------- хранилище
const ensure = (d) => (fs.mkdirSync(d, { recursive: true }), d);
const readJson = (f) => JSON.parse(fs.readFileSync(f, "utf8"));
/** Сеансы сборки (папки <сборка>/<сеанс>), новые последними. */
export function sessions(tag) {
  const d = path.join(BENCH_DIR, tag);
  if (!fs.existsSync(d)) return [];
  return fs.readdirSync(d).filter((n) => fs.existsSync(path.join(d, n, "meta.json"))).sort();
}
/** «сборка» или «сборка/сеанс» → папка сеанса (у сборки — последний сеанс с маршрутом, если просили маршрут). */
export function sessionDir(spec, want = "route") {
  const [tag, ses] = spec.split("/");
  if (ses) return path.join(BENCH_DIR, tag, ses);
  const all = sessions(tag).reverse();
  const hit = all.find((s) => (want === "route" ? fs.readdirSync(path.join(BENCH_DIR, tag, s)).some((f) => f.startsWith("route-")) : fs.existsSync(path.join(BENCH_DIR, tag, s, "look"))));
  if (!hit) return null;
  return path.join(BENCH_DIR, tag, hit);
}
export function builds() {
  if (!fs.existsSync(BENCH_DIR)) return [];
  return fs.readdirSync(BENCH_DIR).filter((n) => fs.statSync(path.join(BENCH_DIR, n)).isDirectory() && sessions(n).length)
    .map((n) => ({ tag: n, last: sessions(n).at(-1), meta: readJson(path.join(BENCH_DIR, n, sessions(n).at(-1), "meta.json")) }))
    .sort((a, b) => (a.meta.at < b.meta.at ? -1 : 1));
}

// ---------------------------------------------------------------- итог сеанса
const r1 = (v) => (v == null ? null : Math.round(v * 10) / 10);
const spread = (vals) => { const v = vals.filter((x) => x != null); if (v.length < 2) return null; const m = v.reduce((a, b) => a + b, 0) / v.length; return m ? r1((Math.max(...v) - Math.min(...v)) / m * 100) : 0; };
// Метрики повторяемости (< 5 %): средний интервал, p95, доля кадров вовремя и цена кадра (GPU, CPU). p50 интервала в ворота
// не входит: с темпом 60 он почти всегда ровно 16,7, без темпа распределение двугорбое (забор очереди: 3 / 20 мс) и
// медиана прыгает между горбами.
const GATE = ["mean", "p95", "share60", "gpu50", "cpu50"];
/** Итог одного прогона маршрута: по отрезкам и в целом. */
export function summarizeRun(rec) {
  const segs = {};
  const all = [];
  for (const s of rec.res.segs) {
    all.push(...s.frames);
    const causes = {};
    // Причина рывка — метка самописца (cause, а для gpu и js — ещё и где: «gpu·opaque», «js·blocks»).
    for (const sp of s.spikes) if (sp) { const k = (sp.cause || "?") + (sp.sub && /^(gpu|js|shader|upload|task)$/.test(sp.cause) ? "·" + sp.sub : ""); causes[k] ||= [0, 0]; causes[k][0]++; if (sp.ms > 50) causes[k][1]++; }
    const g = costStats(s.gpu), c = costStats(s.cpu), m = costStats(s.gpu.map((v, i) => Math.max(v, s.cpu[i])));
    segs[s.name] = { ...frameStats(s.frames), gpu50: g.p50, gpu95: g.p95, cpu50: c.p50, cpu95: c.p95, cost95: m.p95, causes, gpu: s.passes, cpu: s.stages, sub: s.sub || {}, cnt: s.cnt || {}, edits: s.edits, tiles: s.tilesBuilt,
      loadMs: s.loadMs, loadTimeout: s.loadTimeout };
  }
  const causes = {};
  for (const s of Object.values(segs)) for (const [k, [a, b]] of Object.entries(s.causes)) { causes[k] ||= [0, 0]; causes[k][0] += a; causes[k][1] += b; }
  const G = rec.res.segs.flatMap((s) => s.gpu), C = rec.res.segs.flatMap((s) => s.cpu);
  const g = costStats(G), c = costStats(C), m = costStats(G.map((v, i) => Math.max(v, C[i])));
  return { all: { ...frameStats(all), gpu50: g.p50, gpu95: g.p95, cpu50: c.p50, cpu95: c.p95, cost95: m.p95, causes }, segs };
}
/** Итог сеанса по пресету: среднее по чистым повторам, разброс (макс − мин) / среднее, %. */
export function summarizeSession(dir) {
  const files = fs.readdirSync(dir).filter((f) => /^route-.*\.json$/.test(f)).sort();
  const byPreset = {};
  for (const f of files) {
    const rec = readJson(path.join(dir, f));
    if (!rec.res) continue;
    (byPreset[rec.preset] ||= []).push({ rec, sum: summarizeRun(rec) });
  }
  const out = {};
  for (const [p, runs] of Object.entries(byPreset)) {
    const clean = runs.filter((r) => !r.rec.load?.dirty);
    const use = clean.length ? clean : runs;
    const avgOf = (get) => { const v = use.map(get).filter((x) => x != null); return v.length ? Math.round(v.reduce((a, b) => a + b, 0) / v.length * 100) / 100 : null; };
    const block = (get) => {
      const keys = ["mean", "p50", "p95", "p99", "worst1", "share60", "fps", "over25", "over50", "max", "gpu50", "gpu95", "cpu50", "cpu95", "cost95"];
      const m = {}, sp = {};
      for (const k of keys) { m[k] = avgOf((r) => get(r)?.[k]); sp[k] = spread(use.map((r) => get(r)?.[k])); }
      return { ...m, spread: sp };
    };
    const segNames = Object.keys(use[0].sum.segs);
    const avgMap = (get) => { const o = {}; for (const r of use) for (const [k, v] of Object.entries(get(r) || {})) o[k] = (o[k] || 0) + v / use.length; for (const k in o) o[k] = Math.round(o[k] * 100) / 100; return o; };
    const causes = {};
    for (const r of use) for (const [k, [a, b]] of Object.entries(r.sum.all.causes)) { causes[k] ||= [0, 0]; causes[k][0] += a / use.length; causes[k][1] += b / use.length; }
    out[p] = {
      runs: runs.length, clean: clean.length, dirty: runs.filter((r) => r.rec.load?.dirty).map((r) => `${r.rec.rep}: ${r.rec.load.why}`),
      all: block((r) => r.sum.all), causes: Object.fromEntries(Object.entries(causes).map(([k, [a, b]]) => [k, [r1(a), r1(b)]])),
      segs: Object.fromEntries(segNames.map((n) => [n, { ...block((r) => r.sum.segs[n]), gpu: avgMap((r) => r.sum.segs[n]?.gpu), cpu: avgMap((r) => r.sum.segs[n]?.cpu) }])),
      gpu: avgMap((r) => { const o = {}; const ss = Object.values(r.sum.segs); for (const s of ss) for (const [k, v] of Object.entries(s.gpu || {})) o[k] = (o[k] || 0) + v / ss.length; return o; }),
      cpu: avgMap((r) => { const o = {}; const ss = Object.values(r.sum.segs); for (const s of ss) for (const [k, v] of Object.entries(s.cpu || {})) o[k] = (o[k] || 0) + v / ss.length; return o; }),
      sub: avgMap((r) => { const o = {}; const ss = Object.values(r.sum.segs); for (const s of ss) for (const [k, v] of Object.entries(s.sub || {})) o[k] = (o[k] || 0) + v / ss.length; return o; }),
      loadlog: use.map((r) => r.rec.loadlog).filter(Boolean),
      repeat: Math.max(0, ...GATE.map((k) => [block((r) => r.sum.all).spread[k], ...segNames.map((n) => block((r) => r.sum.segs[n]).spread[k])]).flat().filter((x) => x != null)),
      repeatAll: Math.max(0, ...GATE.map((k) => block((r) => r.sum.all).spread[k]).filter((x) => x != null)),
    };
  }
  return out;
}

// ---------------------------------------------------------------- сравнение
// Порог «заметно»: больше 5 % и больше полутора разбросов повторов любой из сборок (иначе — шум).
const LOWER = { mean: 1, p95: 1, p99: 1, worst1: 1, over25: 1, over50: 1, share60: -1, gpu50: 1, gpu95: 1, cpu50: 1, cpu95: 1, cost95: 1 };
export function compare(A, B) {
  const rows = [];
  for (const p of Object.keys(B)) {
    if (!A[p]) continue;
    for (const [seg, a, b] of [["весь маршрут", A[p].all, B[p].all], ...Object.keys(B[p].segs).filter((n) => A[p].segs[n]).map((n) => [n, A[p].segs[n], B[p].segs[n]])]) {
      for (const k of Object.keys(LOWER)) {
        if (a[k] == null || b[k] == null) continue;
        const base = k === "over25" || k === "over50" ? Math.max(a[k], 3) : a[k];
        const d = base ? (b[k] - a[k]) / base * 100 : 0;
        const noise = Math.max(5, 1.5 * Math.max(a.spread?.[k] || 0, b.spread?.[k] || 0));
        const verdict = Math.abs(d) <= noise ? "=" : d * LOWER[k] > 0 ? "хуже" : "лучше";
        rows.push({ preset: p, seg, k, a: a[k], b: b[k], d: r1(d), noise: r1(noise), verdict });
      }
    }
  }
  // Где: проходы GPU и этапы CPU, изменившиеся больше чем на 0,2 мс и 10 %.
  const where = [];
  for (const p of Object.keys(B)) {
    if (!A[p]) continue;
    for (const [kind, ka] of [["GPU", "gpu"], ["CPU", "sub"]]) for (const [k, vb] of Object.entries(B[p][ka] || {})) {
      const va = A[p][ka]?.[k];
      if (va == null || k === "post") continue;
      if (Math.abs(vb - va) > 0.2 && Math.abs(vb - va) / Math.max(va, 0.1) > 0.1) where.push({ preset: p, kind, k, a: va, b: vb, d: Math.round((vb - va) * 100) / 100 });
    }
  }
  return { rows, where: where.sort((x, y) => Math.abs(y.d) - Math.abs(x.d)) };
}
function printCompare(an, bn, A, B) {
  const { rows, where } = compare(A, B);
  console.log(`\nсравнение ${an} → ${bn} (порог: больше 5 % и больше 1,5 разброса повторов)`);
  const main = rows.filter((r) => r.seg === "весь маршрут");
  for (const r of main) console.log(`  ${r.preset.padEnd(8)} ${r.k.padEnd(8)} ${String(r.a).padStart(8)} → ${String(r.b).padStart(8)}  ${(r.d > 0 ? "+" : "") + r.d}%`.padEnd(60) + `  ${r.verdict === "=" ? "в шуме" : r.verdict}`);
  const segs = rows.filter((r) => r.seg !== "весь маршрут" && r.verdict !== "=" && ["mean", "p95", "share60", "over50", "gpu50", "cpu50"].includes(r.k));
  if (segs.length) { console.log("  по отрезкам (заметное):"); for (const r of segs) console.log(`    ${r.preset} ${r.seg.padEnd(11)} ${r.k.padEnd(8)} ${r.a} → ${r.b} (${r.d > 0 ? "+" : ""}${r.d}%) ${r.verdict}`); }
  if (where.length) { console.log("  где (проходы GPU / этапы CPU, мс):"); for (const w of where.slice(0, 12)) console.log(`    ${w.preset} ${w.kind} ${w.k.padEnd(10)} ${w.a} → ${w.b} (${w.d > 0 ? "+" : ""}${w.d})`); }
  const worse = rows.filter((r) => r.verdict === "хуже" && r.seg === "весь маршрут").length, better = rows.filter((r) => r.verdict === "лучше" && r.seg === "весь маршрут").length;
  console.log(`  итог: ${worse ? `ХУЖЕ по ${worse} метрикам` : better ? `лучше по ${better} метрикам` : "без заметной разницы"}`);
  return { rows, where, worse, better };
}

// ---------------------------------------------------------------- метрики вида: попиксельное сравнение
/** Кадр 0 сцены A и B → доля изменившихся пикселей, средняя разница, сетка 16×8 (% по клетке), маска PNG (½ размера). */
export function diffFrames(fa, fb, outPng) {
  const a = decodePng(fs.readFileSync(fa)), b = decodePng(fs.readFileSync(fb));
  if (a.w !== b.w || a.h !== b.h) return { error: `размер ${a.w}×${a.h} ≠ ${b.w}×${b.h}` };
  const W = a.w, H = a.h, GX = 16, GY = 8, grid = new Array(GX * GY).fill(0);
  const mw = W >> 1, mh = H >> 1, mask = new Uint8Array(mw * mh * 4), red = new Uint8Array(mw * mh);
  let changed = 0, sum = 0;
  for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
    const i = (y * W + x) * 4;
    const d = Math.max(Math.abs(a.data[i] - b.data[i]), Math.abs(a.data[i + 1] - b.data[i + 1]), Math.abs(a.data[i + 2] - b.data[i + 2]));
    sum += d;
    const mx = x >> 1, my = y >> 1, m = my < mh && mx < mw ? my * mw + mx : -1;
    if (d > DIFF_T) { changed++; grid[Math.min(GY - 1, (y * GY / H) | 0) * GX + Math.min(GX - 1, (x * GX / W) | 0)]++; if (m >= 0) red[m] = 1; }
    if (m >= 0 && !(x & 1) && !(y & 1)) { const l = (b.data[i] * 77 + b.data[i + 1] * 150 + b.data[i + 2] * 29) >> 8; mask[m * 4] = mask[m * 4 + 1] = mask[m * 4 + 2] = (25 + l * 0.3) | 0; }
  }
  for (let m = 0; m < red.length; m++) if (red[m]) { mask[m * 4] = 255; mask[m * 4 + 1] = 40; mask[m * 4 + 2] = 40; }
  if (outPng) fs.writeFileSync(outPng, encodePng(mw, mh, mask));
  const cell = (W / GX) * (H / GY);
  return { changed: Math.round(changed / (W * H) * 10000) / 100, meanDiff: Math.round(sum / (W * H) * 100) / 100, grid: grid.map((c) => Math.round(c / cell * 1000) / 10), threshold: DIFF_T };
}
const refTag = () => { try { return fs.readFileSync(path.join(BENCH_DIR, "ref.txt"), "utf8").trim(); } catch { return ""; } };

// ---------------------------------------------------------------- прогоны
const log = (...a) => console.log(...a);
async function runRoute(chrome, base, preset, rep, dir, extra) {
  // Темп 60 к/с (?cap=60): интервал кадра — как у пользователя (слот 16,7 мс), рывок — настоящий пропуск. --nocap — без
  // предела (кадр = цена, но пары 3 / 20 мс от забора очереди).
  const q = new URLSearchParams({ ...COMMON, ...PRESETS[preset], route: opt("--only", "all"), loadlog: 1, ...(flag("--nocap") ? {} : { cap: 60 }) });
  for (const [k, v] of new URLSearchParams(extra)) q.set(k, v);
  const u = `${base}/?${q}`;
  const sampler = loadSampler(chrome.mark), g0 = gpuState(), t0 = Date.now();
  let res = null;
  const errs = [], loadLog = [];
  const done = new Promise((ok) => {
    const timer = setTimeout(() => ok(null), 420000);
    chrome.open(u, (kind, text) => {
      if (kind === "console" && text.startsWith("[voxel route]")) { clearTimeout(timer); ok(JSON.parse(text.slice(13).trim())); }
      else if (kind === "console" && text.startsWith("[voxel load]")) { try { const l = JSON.parse(text.slice(12).trim()); loadLog.push([l.t, l.ready, l.tiles, l.fps]); } catch {} }
      else if (kind === "exception" || (kind === "error" && !/favicon|version.json/.test(text))) errs.push(`${kind}: ${text.slice(0, 300)}`);
    });
  });
  res = await done;
  const load = sampler();
  await chrome.blank();
  const loadlog = loadlogWindow(t0, Date.now());
  if (loadlog?.why) { load.dirty = true; load.why = [load.why, `журнал нагрузки: ${loadlog.why}`].filter(Boolean).join("; "); }
  const rec = { preset, rep, url: u, at: new Date().toISOString(), secs: Math.round((Date.now() - t0) / 1000), gpuBefore: g0, gpuAfter: gpuState(), load, loadlog, errs, res };
  fs.writeFileSync(path.join(dir, `route-${preset}-${rep}.json`), JSON.stringify(rec));
  return rec;
}
async function runLook(chrome, base, name, dir, extra) {
  const s = SCENES[name];
  const q = new URLSearchParams({ ...COMMON, ...PRESETS.default, look: name, lookn: LOOK_N, nohud: 1 });
  for (const k of ["x", "y", "z", "yaw", "pitch", "time", "lookdy"]) if (s[k] != null) q.set(k, s[k]);
  if (s.v) q.set("lookv", s.v.join(","));
  if (s.turn) q.set("lookturn", s.turn);
  for (const [k, v] of new URLSearchParams(s.q || "")) q.set(k, v);
  for (const [k, v] of new URLSearchParams(extra)) q.set(k, v);
  const errs = [];
  await chrome.open(`${base}/?${q}`, (kind, text) => { if (kind === "exception" || (kind === "error" && !/favicon|version.json/.test(text))) errs.push(text.slice(0, 300)); });
  const t0 = Date.now();
  let st = null;
  while (Date.now() - t0 < 150000) {
    await sleep(1000);
    st = await chrome.eval("window.__look ? [window.__look.phase, window.__look.i, window.__look.okN, document.title] : null", 10000).catch((e) => String(e.message || e));
    if (process.env.BENCH_DEBUG) console.log("   ", Math.round((Date.now() - t0) / 1000), JSON.stringify(st));
    if (Array.isArray(st) && st[0] === "done") { st = "done"; break; }
  }
  if (st !== "done") { await chrome.blank(); return { name, error: `не дождались кадров (${st}) ${errs.slice(0, 2).join(" | ")}` }; }
  const r = await chrome.eval("window.__look.result", 120000);
  await chrome.blank();
  const ld = ensure(path.join(dir, "look"));
  fs.writeFileSync(path.join(ld, `${name}.png`), Buffer.from(r.frame0, "base64"));
  fs.writeFileSync(path.join(ld, `${name}.jpg`), Buffer.from(r.preview, "base64"));
  fs.writeFileSync(path.join(ld, `${name}-heat.png`), Buffer.from(r.heat, "base64"));
  if (r.frameN) fs.writeFileSync(path.join(ld, `${name}-last.png`), Buffer.from(r.frameN, "base64"));
  const rec = { name, about: s.about, url: `?${q}`, flicker: r.flicker, activity: r.activity, grid: r.grid, gridW: r.gridW, gridH: r.gridH,
    frames: r.frames, w: r.w, h: r.h, loadMs: r.loadMs, loadTimeout: r.loadTimeout, steps: r.steps, threshold: r.threshold, errs };
  fs.writeFileSync(path.join(ld, `${name}.json`), JSON.stringify(rec));
  return rec;
}

// Журнал нагрузки ноутбука (tools/loadlog.ps1, Q15): строки за время прогона — среднее и максимум по группам. Группа
// stand — все headless-Chrome стендов, и наш тоже, поэтому в «грязь» идут cargo, Chrome пользователя и прочее.
const LOADLOG = path.resolve(BENCH_DIR, "..", "_loadlog");
export function loadlogWindow(t0, t1) {
  const rows = [];
  for (const d of new Set([new Date(t0).toISOString().slice(0, 10), new Date(t1).toISOString().slice(0, 10)])) {
    const f = path.join(LOADLOG, `${d}.jsonl`);
    if (!fs.existsSync(f)) continue;
    for (const line of fs.readFileSync(f, "utf8").split(/\r?\n/)) {
      if (!line) continue;
      try { const r = JSON.parse(line), t = Date.parse(r.ts); if (t >= t0 && t <= t1) rows.push(r); } catch {}
    }
  }
  if (!rows.length) return null;
  const agg = (get) => { const v = rows.map(get).filter((x) => x != null); return v.length ? [r1(v.reduce((a, b) => a + b, 0) / v.length), Math.max(...v)] : null; };
  const g = {}, g3 = {};
  for (const k of ["cargo", "stand", "chrome", "node", "other"]) g[k] = agg((r) => r.g?.[k]);
  for (const k of ["chrome", "stand", "other"]) g3[k] = agg((r) => r.gpu3d?.[k]);
  const why = [g.cargo?.[0] >= 5 ? `cargo ${g.cargo[0]} % процессора` : "", g.chrome?.[0] >= 5 ? `Chrome пользователя ${g.chrome[0]} %` : "",
    g3.chrome?.[0] >= 10 ? `Chrome пользователя на видеокарте ${g3.chrome[0]} %` : "", g3.other?.[0] >= 15 ? `чужие на видеокарте ${g3.other[0]} %` : ""].filter(Boolean).join("; ");
  return { n: rows.length, cpu: agg((r) => r.cpu), g, gpu3d: g3, nv: agg((r) => r.nv?.util), why };
}

function journal(line) { ensure(BENCH_DIR); fs.appendFileSync(path.join(BENCH_DIR, "journal.jsonl"), JSON.stringify(line) + "\n"); }
const fmtSpread = (v) => (v == null ? "—" : `${v}%`);
function printSession(tag, sum) {
  for (const [p, s] of Object.entries(sum)) {
    const a = s.all;
    log(`\n${tag} · ${p}: прогонов ${s.runs} (чистых ${s.clean})${s.dirty.length ? " · грязные: " + s.dirty.join("; ") : ""}`);
    log(`  весь маршрут: интервал сред ${a.mean} p95 ${a.p95} p99 ${a.p99} мс · 1 % худших ${a.worst1} мс · вовремя (≤20,8) ${a.share60} % · ` +
      `рывков >25 ${a.over25} >50 ${a.over50} · к/с ${a.fps} · цена: GPU ${a.gpu50}/${a.gpu95} CPU ${a.cpu50}/${a.cpu95} мс (p50/p95)`);
    log(`  разброс повторов: сред ${fmtSpread(a.spread.mean)} p95 ${fmtSpread(a.spread.p95)} вовремя ${fmtSpread(a.spread.share60)} GPU ${fmtSpread(a.spread.gpu50)} CPU ${fmtSpread(a.spread.cpu50)} · ` +
      `худший по отрезкам ${fmtSpread(s.repeat)} (ворота < 5 %)`);
    log(`  рывки по причинам (>25 / >50 мс, среднее на прогон): ${Object.entries(s.causes).map(([k, [x, y]]) => `${k} ${x}/${y}`).join(", ") || "—"}`);
    for (const [n, g] of Object.entries(s.segs))
      log(`    ${n.padEnd(11)} сред ${String(g.mean).padStart(6)} p95 ${String(g.p95).padStart(6)} вовремя ${String(g.share60).padStart(5)}% >25 ${String(g.over25).padStart(4)} >50 ${String(g.over50).padStart(3)} · GPU ${g.gpu50}/${g.gpu95} CPU ${g.cpu50}/${g.cpu95} · ` +
        `GPU ${Object.entries(g.gpu).filter(([k]) => !["post", "total"].includes(k) && g.gpu[k] >= 0.3).map(([k, v]) => `${k} ${v}`).join(" ")} · разброс сред ${fmtSpread(g.spread.mean)} p95 ${fmtSpread(g.spread.p95)}`);
  }
}

async function cmdRun(withRoute, withLook) {
  const root = path.resolve(opt("--root", path.join(REPO, "web")));
  if (!fs.existsSync(path.join(root, "pkg", "voxel_bg.wasm"))) { console.log(`нет сборки WASM в ${root}/pkg — сначала bash build.sh`); process.exit(2); }
  if (!fs.existsSync(path.join(root, "game", "route.js"))) { console.log(`в ${root} нет web/game/route.js — сборка старее маршрута (Q16)`); process.exit(2); }
  const tag = opt("--tag", buildName(root)), extra = opt("--q", "");
  const presets = opt("--presets", "default,heavy").split(","), reps = +opt("--reps", 2);
  const scenes = opt("--scenes", Object.keys(SCENES).join(",")).split(",");
  const d0 = new Date(), p2 = (v) => String(v).padStart(2, "0");
  const ses = `${d0.getFullYear()}${p2(d0.getMonth() + 1)}${p2(d0.getDate())}-${p2(d0.getHours())}${p2(d0.getMinutes())}`; // местное время
  const dir = ensure(path.join(BENCH_DIR, tag, ses));
  const minutes = (withRoute ? presets.length * reps * 2.6 : 0) + (withLook ? scenes.length * 0.5 : 0) + 2;
  log(`bench: сборка «${tag}» (${root}) → ${dir}`);
  log(`  план: ${withRoute ? `маршрут × ${presets.join(", ")} × ${reps}` : ""}${withRoute && withLook ? " + " : ""}${withLook ? `вид: ${scenes.join(", ")}` : ""}; ~${Math.round(minutes)} мин; замок ${LOCK}`);
  await takeLock(`bench ${tag} (${path.basename(REPO)})`, minutes);
  // Снимок web/ (≈ 4 МБ) на время сеанса: правка или сборка в папке во время замера не попадёт в середину сеанса.
  const snap = fs.mkdtempSync(path.join(os.tmpdir(), "voxel-bench-web-"));
  fs.cpSync(root, snap, { recursive: true, filter: (f) => !/[\\/]replays([\\/]|$)/.test(f) });
  const srv = await serve(snap);
  const meta = { tag, session: ses, root, at: new Date().toISOString(), extra, presets, reps, scenes: withLook ? scenes : [], note: opt("--note", ""),
    gpu: null, quiet: [] };
  const waitMin = +opt("--wait", 20);
  try {
    if (withRoute) {
      for (let rep = 1; rep <= reps; rep++) for (const p of presets) {
        const q = await waitQuiet(waitMin, log);
        meta.quiet.push({ run: `${p}-${rep}`, ...q });
        if (!q.quiet) log(`  тишины нет за ${waitMin} мин (${q.why || "нагрузка не измерена"}) — меряю как есть, прогон будет помечен`);
        const chrome = await openChrome();
        try {
          log(`▸ маршрут ${p} #${rep} …`);
          const rec = await runRoute(chrome, srv.base, p, rep, dir, extra);
          if (!rec.res) { log(`  нет итога за 7 мин: ${rec.errs.slice(0, 3).join(" | ")}`); continue; }
          if (!q.quiet && rec.load) { rec.load.dirty = true; rec.load.why = [rec.load.why, `до прогона: ${q.why || "тишины не было"}`].filter(Boolean).join("; "); fs.writeFileSync(path.join(dir, `route-${p}-${rep}.json`), JSON.stringify(rec)); }
          meta.gpu ||= rec.res.gpu;
          const s = summarizeRun(rec).all;
          log(`  ${rec.secs} с · интервал сред ${s.mean} p95 ${s.p95} p99 ${s.p99} · вовремя ${s.share60} % · >25 ${s.over25} >50 ${s.over50} · GPU ${s.gpu50}/${s.gpu95} CPU ${s.cpu50}/${s.cpu95}` +
            ` · чужое: проц ${rec.load.cpu.avg}/${rec.load.cpu.max} %, видеокарта ${rec.load.gpu.avg}/${rec.load.gpu.max} %${rec.load.dirty ? " · ГРЯЗНЫЙ: " + rec.load.why : ""}` +
            (rec.errs.length ? ` · ошибок страницы ${rec.errs.length}` : ""));
        } finally { await chrome.close(); }
      }
    }
    if (withLook) {
      const q = await waitQuiet(waitMin, log);
      meta.quiet.push({ run: "look", ...q });
      // Каждая сцена — в свежем Chrome: второй переход в том же Chrome после выдачи результата (десятки МБ) зависал.
      for (const n of scenes) {
        if (!SCENES[n]) { log(`  нет сцены ${n}`); continue; }
        log(`▸ вид ${n} …`);
        const chrome = await openChrome();
        try {
          const r = await runLook(chrome, srv.base, n, dir, extra);
          log(r.error ? `  ${r.error}` : `  мерцание ${r.flicker} ‰ · жизнь кадра ${r.activity} · загрузка ${Math.round(r.loadMs / 100) / 10} с${r.loadTimeout ? " (не догрузился)" : ""}${r.steps !== r.frames ? ` · кадров ${r.steps}/${r.frames}!` : ""}`);
        } finally { await chrome.close(); }
      }
    }
  } finally { srv.close(); releaseLock(); try { fs.rmSync(snap, { recursive: true, force: true }); } catch {} }
  fs.writeFileSync(path.join(dir, "meta.json"), JSON.stringify(meta, null, 1));
  // Итог, журнал, сравнение с эталоном и прошлой сборкой, страница.
  const sum = withRoute ? summarizeSession(dir) : null;
  if (sum) {
    fs.writeFileSync(path.join(dir, "summary.json"), JSON.stringify(sum));
    printSession(tag, sum);
    for (const [p, s] of Object.entries(sum)) journal({ at: meta.at, tag, session: ses, preset: p, runs: s.runs, clean: s.clean, repeat: s.repeat,
      mean: s.all.mean, p95: s.all.p95, gpu50: s.all.gpu50, cpu50: s.all.cpu50, p99: s.all.p99, worst1: s.all.worst1, share60: s.all.share60, over25: s.all.over25, over50: s.all.over50,
      segs: Object.fromEntries(Object.entries(s.segs).map(([n, g]) => [n, [g.mean, g.p95, g.share60]])), extra, note: meta.note });
  }
  if (withLook) lookSummary(tag, dir);
  const ref = refTag();
  if (sum && ref && ref !== tag) { const rd = sessionDir(ref); if (rd) printCompare(ref, tag, summarizeSession(rd), sum); }
  const page = writePage(defaultPageTags(tag));
  log(`\nстраница: ${page}`);
}
function lookSummary(tag, dir) {
  const ld = path.join(dir, "look");
  if (!fs.existsSync(ld)) return;
  const ref = refTag(), rd = ref && ref !== tag ? sessionDir(ref, "look") : null;
  log(`\nвид ${tag}${rd ? ` (сравнение с эталоном ${ref})` : ""}:`);
  for (const f of fs.readdirSync(ld).filter((f) => /^[a-z]+\.json$/.test(f))) {
    const r = readJson(path.join(ld, f));
    let line = `  ${r.name.padEnd(11)} мерцание ${String(r.flicker).padStart(6)} ‰`;
    if (rd && fs.existsSync(path.join(rd, "look", `${r.name}.png`))) {
      const d = diffFrames(path.join(rd, "look", `${r.name}.png`), path.join(ld, `${r.name}.png`), path.join(ld, `${r.name}-diff.png`));
      fs.writeFileSync(path.join(ld, `${r.name}-diff.json`), JSON.stringify({ ref, ...d }));
      const rr = readJson(path.join(rd, "look", `${r.name}.json`));
      line += ` (эталон ${rr.flicker})` + (d.error ? ` · ${d.error}` : ` · изменилось пикселей ${d.changed} % (средн. разница ${d.meanDiff})`);
    }
    log(line);
  }
}
function defaultPageTags(tag) {
  const all = builds().map((b) => b.tag);
  const ref = refTag();
  const list = [...new Set([ref && all.includes(ref) ? ref : null, ...all.slice(-3), tag].filter(Boolean))];
  return list.slice(-4);
}

// ---------------------------------------------------------------- команды
// Команды — только при запуске файла (page.mjs и проверки импортируют отсюда функции).
const MAIN = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (!MAIN) {
} else if (cmd === "run") await cmdRun(!flag("--no-route"), !flag("--no-look"));
else if (cmd === "look") await cmdRun(false, true);
else if (cmd === "compare") {
  const [a, b] = pos;
  const da = sessionDir(a), db = sessionDir(b);
  if (!da || !db) { console.log("нет прогонов маршрута:", !da ? a : b); process.exit(2); }
  const A = summarizeSession(da), B = summarizeSession(db);
  printSession(a, A); printSession(b, B);
  const r = printCompare(a, b, A, B);
  const la = sessionDir(a, "look"), lb = sessionDir(b, "look");
  if (la && lb) {
    console.log("\nвид:");
    for (const f of fs.readdirSync(path.join(lb, "look")).filter((f) => /^[a-z]+\.json$/.test(f))) {
      const n = f.slice(0, -5), ra = path.join(la, "look", f);
      if (!fs.existsSync(ra)) continue;
      const d = diffFrames(path.join(la, "look", `${n}.png`), path.join(lb, "look", `${n}.png`), path.join(lb, "look", `${n}-vs-${a.replace("/", "_")}.png`));
      console.log(`  ${n.padEnd(11)} мерцание ${readJson(ra).flicker} → ${readJson(path.join(lb, "look", f)).flicker} ‰ · изменилось пикселей ${d.changed ?? d.error} %`);
    }
  }
  process.exit(r.worse ? 1 : 0);
} else if (cmd === "page") log(writePage(pos.length ? pos : defaultPageTags(builds().at(-1)?.tag)));
else if (cmd === "ref") { ensure(BENCH_DIR); fs.writeFileSync(path.join(BENCH_DIR, "ref.txt"), pos[0] || ""); log(`эталон: ${pos[0] || "нет"}`); }
else if (cmd === "list") for (const b of builds()) log(`${b.tag.padEnd(16)} ${sessions(b.tag).join(" ")}  ${b.meta.note || ""}`);
else if (cmd === "status") {
  let lock = null; try { lock = { ...JSON.parse(fs.readFileSync(LOCK, "utf8")), age: Math.round((Date.now() - fs.statSync(LOCK).mtimeMs) / 1000) }; } catch {}
  log(`замок ${LOCK}: ${lock ? `${lock.who}, pid ${lock.pid}, обновлён ${lock.age} с назад` : "нет"}`);
  log("нагрузка:", JSON.stringify(await foreignLoad()), "видеокарта:", JSON.stringify(gpuState()));
} else { console.log("команды: run | look | compare A B | page [A B …] | ref <сборка> | list | status"); process.exit(2); }
