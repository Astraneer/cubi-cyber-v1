// Общее для perf/bench/*.mjs (Q16): папка замеров, замок .bench.lock, тишина машины, Chrome на видеокарте по CDP,
// статика, PNG без зависимостей, статистика кадров.
import { spawn, execFileSync, execFile } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import http from "node:http";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";

export const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO = path.resolve(HERE, "../..");
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const git = (...a) => execFileSync("git", ["-C", REPO, ...a], { encoding: "utf8" }).trim();
// Общий .git всех рабочих папок: замок и замеры видны из любой папки.
export const GIT_COMMON = path.resolve(REPO, git("rev-parse", "--git-common-dir"));
export const MAIN_REPO = path.dirname(GIT_COMMON);
// Замеры всех сборок — одна общая папка вне git (рядом с рабочими папками агентов): voxel-wt/_bench.
export const BENCH_DIR = process.env.BENCH_DIR || path.resolve(MAIN_REPO, "..", "voxel-wt", "_bench");
export const LOCK = path.join(GIT_COMMON, ".bench.lock");
const STAND_LOCK = path.join(os.tmpdir(), "voxel-stand.lock"); // замок perf/stand.mjs — его тоже держим
export const CHROME = "C:/Program Files/Google/Chrome/Application/chrome.exe";

/** Имя сборки по умолчанию: короткий коммит папки web + «+» при незакоммиченных правках web/ и crates/. */
export function buildName(root) {
  const dir = path.resolve(root, "..");
  try {
    const sha = execFileSync("git", ["-C", dir, "rev-parse", "--short", "HEAD"], { encoding: "utf8" }).trim();
    const dirty = execFileSync("git", ["-C", dir, "status", "--porcelain", "--", "web", "crates"], { encoding: "utf8" }).trim();
    return sha + (dirty ? "+" : "");
  } catch { return "unknown"; }
}

// ---------------------------------------------------------------- замок
// .bench.lock в общем .git: JSON {pid, who, since, until}; живой, пока файл моложе LOCK_STALE (стенд обновляет его раз
// в 10 с). tools/check.sh и build.sh (tools/ci/bench_wait.sh) ждут, пока он есть. Второй стенд ждёт первого.
const LOCK_STALE = 60000;
let lockTimer = null, lockInfo = null;
const alive = (pid) => { try { process.kill(pid, 0); return true; } catch { return false; } };
function lockFresh() {
  try {
    const st = fs.statSync(LOCK), j = JSON.parse(fs.readFileSync(LOCK, "utf8"));
    if (Date.now() - st.mtimeMs > LOCK_STALE) return null;
    if (j.host === os.hostname() && !alive(j.pid)) return null;
    return j;
  } catch { return null; }
}
export async function takeLock(who, minutes) {
  let said = false;
  for (;;) {
    const other = lockFresh();
    if (!other) {
      try { fs.rmSync(LOCK, { force: true }); } catch {}
      lockInfo = { pid: process.pid, host: os.hostname(), who, since: new Date().toISOString(), until: new Date(Date.now() + minutes * 60000).toISOString() };
      try { fs.writeFileSync(LOCK, JSON.stringify(lockInfo), { flag: "wx" }); break; } catch {}
    } else if (!said) { console.log(`  замер уже идёт (${other.who}, pid ${other.pid}, до ~${other.until?.slice(11, 16)}) — жду`); said = true; }
    await sleep(3000);
  }
  // Замок perf/stand.mjs (tmp/voxel-stand.lock: «pid время»): чужой живой — ждём, свой — обновляем время.
  for (said = false; ;) {
    try { fs.writeFileSync(STAND_LOCK, `${process.pid} ${Date.now()}`, { flag: "wx" }); break; } catch {}
    let o = null;
    try { const [pid, at] = fs.readFileSync(STAND_LOCK, "utf8").split(" ").map(Number); o = { pid, at }; } catch {}
    if (!o || !alive(o.pid) || Date.now() - o.at > 2400000) { try { fs.rmSync(STAND_LOCK); } catch {} continue; }
    if (!said) { console.log(`  стенд perf/stand.mjs (pid ${o.pid}) занимает видеокарту — жду`); said = true; }
    await sleep(3000);
  }
  lockTimer = setInterval(() => {
    try { fs.writeFileSync(LOCK, JSON.stringify(lockInfo)); } catch {}
    try { fs.writeFileSync(STAND_LOCK, `${process.pid} ${Date.now()}`); } catch {}
  }, 10000);
  const drop = () => releaseLock();
  process.on("exit", drop);
  for (const s of ["SIGINT", "SIGTERM", "SIGBREAK"]) process.on(s, () => { drop(); process.exit(130); });
}
export function releaseLock() {
  if (lockTimer) { clearInterval(lockTimer); lockTimer = null; }
  try { const j = JSON.parse(fs.readFileSync(LOCK, "utf8")); if (j.pid === process.pid) fs.rmSync(LOCK); } catch {}
  try { if (fs.readFileSync(STAND_LOCK, "utf8").startsWith(`${process.pid} `)) fs.rmSync(STAND_LOCK); } catch {}
}

// ---------------------------------------------------------------- тишина машины
export function gpuState() {
  try {
    const s = execFileSync("nvidia-smi", ["--query-gpu=temperature.gpu,utilization.gpu,clocks.gr,clocks.mem,power.draw", "--format=csv,noheader,nounits"],
      { encoding: "utf8" }).trim().split(/,\s*/).map(Number);
    return { temp: s[0], util: s[1], mhz: s[2], memMhz: s[3], watt: s[4] };
  } catch { return null; }
}
/** Чужая нагрузка за ~1 с (quiet.ps1): { cpu, gpu, top: [[имя, %]] } или null. */
export function foreignLoad(mark = "") {
  const a = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path.join(HERE, "quiet.ps1"), "-Own", String(process.pid)];
  if (mark) a.push("-Mark", mark);
  return new Promise((ok) => execFile("powershell", a, { encoding: "utf8", timeout: 20000 }, (e, o) => {
    try { ok(JSON.parse(String(o).trim().split("\n").pop())); } catch { ok(null); }
  }));
}
// Пороги тишины (docs/areas/bench.md): чужой процессор < CPU_Q % машины, чужая видеокарта < GPU_Q %, видеокарта ≤ TEMP_Q °C.
export const CPU_Q = 15, GPU_Q = 10, TEMP_Q = 75, CPU_DIRTY = 20, GPU_DIRTY = 15;
/** Ждать тишины: три замера подряд ниже порогов и нет браузерной проверки check.sh; не дольше waitMin. */
export async function waitQuiet(waitMin, log = console.log) {
  const t0 = Date.now(), bl = path.join(GIT_COMMON, "voxel-checks", "browser.lock");
  let n = 0, said = "", last = null;
  for (;;) {
    const l = await foreignLoad(), gs = gpuState(), check = fs.existsSync(bl);
    last = { ...l, temp: gs?.temp, check };
    const why = check ? "идёт браузерная проверка check.sh" : !l ? "" : l.cpu >= CPU_Q ? `чужой процессор ${l.cpu} % (${l.top.map((x) => x.join(" ")).join(", ")})`
      : l.gpu >= GPU_Q ? `чужая видеокарта ${l.gpu} %` : gs && gs.temp > TEMP_Q ? `видеокарта ${gs.temp} °C` : "";
    n = why ? 0 : n + 1;
    if (n >= 3) return { quiet: true, waited: Math.round((Date.now() - t0) / 1000), last };
    if (why && why.split(" ")[0] !== said.split(" ")[0]) { log(`  не тихо: ${why} — жду`); said = why; }
    if (Date.now() - t0 > waitMin * 60000) return { quiet: false, waited: Math.round((Date.now() - t0) / 1000), why, last };
    await sleep(1000);
  }
}
/** Нагрузка во время прогона: замер каждые ~4 с; stop() → { samples, cpu: {avg, max}, gpu: {avg, max}, top, dirty, why }. */
export function loadSampler(mark) {
  const s = [];
  let stop = false;
  (async () => { while (!stop) { const l = await foreignLoad(mark); if (l) s.push({ t: Date.now(), ...l, temp: gpuState()?.temp }); await sleep(500); } })();
  return () => {
    stop = true;
    const avg = (k) => (s.length ? Math.round(s.reduce((a, x) => a + x[k], 0) / s.length * 10) / 10 : null);
    const max = (k) => (s.length ? Math.max(...s.map((x) => x[k])) : null);
    const top = {};
    for (const x of s) for (const [n, v] of x.top) top[n] = Math.max(top[n] || 0, v);
    const badCpu = s.filter((x) => x.cpu >= CPU_DIRTY).length, badGpu = s.filter((x) => x.gpu >= GPU_DIRTY).length;
    const why = [badCpu >= 2 ? `чужой процессор ≥ ${CPU_DIRTY} % в ${badCpu} замерах` : "", badGpu >= 2 ? `чужая видеокарта ≥ ${GPU_DIRTY} % в ${badGpu} замерах` : ""].filter(Boolean).join("; ");
    return { samples: s.length, cpu: { avg: avg("cpu"), max: max("cpu") }, gpu: { avg: avg("gpu"), max: max("gpu") }, tempMax: max("temp"),
      top: Object.entries(top).sort((a, b) => b[1] - a[1]).slice(0, 4), dirty: !!why, why, series: s.map((x) => [x.t, x.cpu, x.gpu]) };
  };
}

// ---------------------------------------------------------------- статика: web/ сборки + своя папка замеров
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".mjs": "text/javascript; charset=utf-8",
  ".wasm": "application/wasm", ".json": "application/json", ".png": "image/png", ".jpg": "image/jpeg", ".css": "text/css",
  ".svg": "image/svg+xml", ".ogg": "audio/ogg", ".wav": "audio/wav", ".mp3": "audio/mpeg", ".ttf": "font/ttf", ".woff2": "font/woff2", ".txt": "text/plain" };
export async function serve(root) {
  root = path.resolve(root);
  const server = http.createServer((req, res) => {
    let p = decodeURIComponent(new URL(req.url, "http://x").pathname);
    if (p.endsWith("/")) p += "index.html";
    const f = path.resolve(root, "." + p);
    if (!f.startsWith(root)) { res.writeHead(403).end(); return; }
    fs.readFile(f, (err, data) => {
      if (err) { res.writeHead(404).end(); return; }
      res.writeHead(200, { "Content-Type": TYPES[path.extname(f).toLowerCase()] || "application/octet-stream", "Cache-Control": "no-store" });
      res.end(data);
    });
  });
  await new Promise((ok) => server.listen(0, "127.0.0.1", 511, ok));
  return { base: `http://127.0.0.1:${server.address().port}`, close: () => server.close() };
}

// ---------------------------------------------------------------- Chrome на видеокарте
// Окно 1720×720 × масштаб 2 = холст 3440×1440 (экран пользователя), ANGLE D3D11; без vsync — кадр = его цена.
export async function openChrome({ vsync = false, mark = `voxel-bench-${process.pid}-` } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), mark));
  const flags = ["--headless=new", "--use-angle=d3d11", "--ignore-gpu-blocklist", "--window-size=1720,720", "--force-device-scale-factor=2",
    "--disable-background-timer-throttling", "--disable-renderer-backgrounding", "--disable-backgrounding-occluded-windows",
    "--no-first-run", "--autoplay-policy=no-user-gesture-required", "--mute-audio", `--user-data-dir=${dir}`, "--remote-debugging-port=0"];
  if (!vsync) flags.push("--disable-gpu-vsync", "--disable-frame-rate-limit");
  const ch = spawn(CHROME, [...flags, "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
  let port = null;
  ch.stderr.on("data", (b) => { const m = /DevTools listening on ws:\/\/127\.0\.0\.1:(\d+)/.exec(String(b)); if (m) port = +m[1]; });
  for (let i = 0; i < 100 && !port; i++) await sleep(100);
  if (!port) throw new Error("Chrome: нет порта отладки");
  let page = null;
  for (let i = 0; i < 50 && !page; i++) {
    try { page = (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).find((t) => t.type === "page"); } catch {}
    if (!page) await sleep(100);
  }
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((r) => { ws.onopen = r; });
  let id = 0, sink = null;
  const pending = new Map();
  const send = (method, params = {}) => new Promise((ok) => { const n = ++id; pending.set(n, ok); ws.send(JSON.stringify({ id: n, method, params })); });
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id && pending.has(d.id)) { pending.get(d.id)(d.result || { error: d.error }); pending.delete(d.id); return; }
    if (!sink) return;
    if (d.method === "Runtime.consoleAPICalled") sink("console", d.params.args.map((a) => a.value ?? a.description ?? "").join(" "), d.params.executionContextId);
    else if (d.method === "Runtime.exceptionThrown") sink("exception", d.params.exceptionDetails.exception?.description || d.params.exceptionDetails.text || "");
    else if (d.method === "Log.entryAdded" && d.params.entry.level === "error") sink("error", `${d.params.entry.text} ${d.params.entry.url || ""}`);
  };
  await send("Runtime.enable"); await send("Log.enable");
  // Процессы своего Chrome — повыше приоритетом (против чужих сборок на той же машине), как perf/stand.mjs.
  const boost = () => execFile("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command",
    `Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" | Where-Object { $_.CommandLine -like '*${mark}*' } | ForEach-Object { try { (Get-Process -Id $_.ProcessId).PriorityClass = 'High' } catch {} }`],
    { timeout: 15000 }, () => {});
  return {
    mark,
    send,
    /** Открыть адрес; on(kind, text) — консоль страницы (строки прошлой страницы отсеиваются по контексту). */
    async open(u, on = () => {}) {
      sink = on;
      await Promise.race([send("Page.navigate", { url: u }), sleep(15000)]);
      boost(); setTimeout(boost, 3000); setTimeout(boost, 8000);
    },
    async eval(expr, ms = 30000) {
      const r = await Promise.race([send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true }), sleep(ms).then(() => null)]);
      if (!r) throw new Error("eval: нет ответа за " + ms + " мс");
      if (r.exceptionDetails) throw new Error("eval: " + (r.exceptionDetails.exception?.description || r.exceptionDetails.text));
      return r.result?.value;
    },
    async blank() { sink = null; await Promise.race([send("Page.navigate", { url: "about:blank" }), sleep(15000)]); },
    async close() {
      sink = null;
      try { ws.close(); } catch {}
      try { execFileSync("taskkill", ["/PID", String(ch.pid), "/T", "/F"], { stdio: "ignore" }); } catch { try { ch.kill(); } catch {} }
      await sleep(1500); try { fs.rmSync(dir, { recursive: true, force: true }); } catch {}
    },
  };
}

// ---------------------------------------------------------------- PNG (RGB/RGBA 8 бит) без зависимостей
const CRC = new Int32Array(256).map((_, n) => { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; return c; });
const crc32 = (b) => { let c = -1; for (let i = 0; i < b.length; i++) c = CRC[(c ^ b[i]) & 255] ^ (c >>> 8); return (c ^ -1) >>> 0; };
export function decodePng(buf) {
  let p = 8, w = 0, h = 0, ct = 0;
  const idat = [];
  while (p < buf.length) {
    const len = buf.readUInt32BE(p), type = buf.toString("latin1", p + 4, p + 8), d = buf.subarray(p + 8, p + 8 + len);
    if (type === "IHDR") { w = d.readUInt32BE(0); h = d.readUInt32BE(4); ct = d[9]; if (d[8] !== 8 || d[12] !== 0) throw new Error("PNG: только 8 бит без чересстрочности"); }
    else if (type === "IDAT") idat.push(d);
    else if (type === "IEND") break;
    p += 12 + len;
  }
  const bpp = ct === 6 ? 4 : ct === 2 ? 3 : 0;
  if (!bpp) throw new Error("PNG: тип цвета " + ct);
  const raw = zlib.inflateSync(Buffer.concat(idat)), stride = w * bpp, out = new Uint8Array(w * h * 4);
  let prev = new Uint8Array(stride), cur = new Uint8Array(stride);
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)], row = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? cur[x - bpp] : 0, b = prev[x], c = x >= bpp ? prev[x - bpp] : 0;
      let v = row[x];
      if (f === 1) v += a; else if (f === 2) v += b; else if (f === 3) v += (a + b) >> 1;
      else if (f === 4) { const pp = a + b - c, pa = Math.abs(pp - a), pb = Math.abs(pp - b), pc = Math.abs(pp - c); v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c; }
      cur[x] = v & 255;
    }
    for (let x = 0, o = y * w * 4; x < w; x++, o += 4) { out[o] = cur[x * bpp]; out[o + 1] = cur[x * bpp + 1]; out[o + 2] = cur[x * bpp + 2]; out[o + 3] = bpp === 4 ? cur[x * bpp + 3] : 255; }
    [prev, cur] = [cur, prev];
  }
  return { w, h, data: out };
}
export function encodePng(w, h, rgba) {
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) { const i = (y * w + x) * 4, o = y * (w * 3 + 1) + 1 + x * 3; raw[o] = rgba[i]; raw[o + 1] = rgba[i + 1]; raw[o + 2] = rgba[i + 2]; }
  const chunk = (type, d) => { const b = Buffer.alloc(12 + d.length); b.writeUInt32BE(d.length, 0); b.write(type, 4, "latin1"); d.copy(b, 8); b.writeUInt32BE(crc32(b.subarray(4, 8 + d.length)), 8 + d.length); return b; };
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(raw, { level: 6 })), chunk("IEND", Buffer.alloc(0))]);
}

// ---------------------------------------------------------------- статистика кадров
// Маршрут идёт с темпом 60 к/с (?cap=60, как у пользователя: 120 Гц, делитель 2). Кадр «вовремя», если интервал ≤ 20,8 мс
// — середина между слотом 16,7 и пропуском (25 мс на 120 Гц); дрожь таймера в 1–2 мс не считается пропуском.
export const BUDGET = 1000 / 60 * 1.25;
/** Цена кадров (не интервал): p50/p95 ряда, нули (нет замера) — мимо. */
export function costStats(v) {
  const a = Float64Array.from(v.filter((x) => x > 0)).sort();
  if (!a.length) return { p50: null, p95: null };
  const at = (p) => Math.round(a[Math.min(a.length - 1, Math.floor(p * a.length))] * 100) / 100;
  return { p50: at(0.5), p95: at(0.95) };
}
export function frameStats(ms) {
  const a = Float64Array.from(ms).sort();
  const n = a.length;
  if (!n) return null;
  const at = (p) => a[Math.min(n - 1, Math.floor(p * n))];
  const sum = a.reduce((s, v) => s + v, 0), k = Math.max(1, Math.floor(n * 0.01));
  let worst = 0; for (let i = n - k; i < n; i++) worst += a[i];
  const r = (v) => Math.round(v * 100) / 100;
  return { n, mean: r(sum / n), p50: r(at(0.5)), p95: r(at(0.95)), p99: r(at(0.99)), worst1: r(worst / k), max: r(a[n - 1]),
    share60: Math.round(a.filter((v) => v <= BUDGET).length / n * 1000) / 10, fps: Math.round(n / (sum / 1000) * 10) / 10,
    over25: a.filter((v) => v > 25).length, over50: a.filter((v) => v > 50).length };
}
