// HTML-страница сравнения сборок (Q16): BENCH_DIR/index.html — итоги маршрута по пресетам, отрезки, график кадров по
// времени маршрута, рывки по причинам, GPU по проходам и CPU по этапам, повторяемость, метрики вида (мерцание,
// картинки было/стало, маска изменений, карта мерцания). Картинки — файлами рядом (относительные пути), без библиотек.
//   node perf/bench/route.mjs page [A B …]   (первая сборка — база, с ней сравниваются остальные)
import fs from "node:fs";
import path from "node:path";
import { BENCH_DIR } from "./lib.mjs";
import { sessionDir, summarizeSession, compare, diffFrames, SCENES } from "./route.mjs";

const esc = (t) => String(t ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const rel = (f) => path.relative(BENCH_DIR, f).split(path.sep).join("/");
const SEG_RU = { forest: "лес с мобами", crossroads: "Перекрёсток, витрина", mountain: "горы, даль", water: "вода", flight: "быстрый полёт",
  build: "ломание и ставление", night: "ночь со светом" };
const PRESET_RU = { default: "«По умолчанию»", heavy: "«Тяжелее»" };
const METRICS = [
  ["share60", "кадров вовремя", "%"], ["over25", "рывков > 25 мс", ""], ["over50", "рывков > 50 мс", ""], ["mean", "интервал, сред", "мс"],
  ["p95", "интервал p95", "мс"], ["worst1", "1 % худших", "мс"], ["gpu50", "GPU кадра p50", "мс"], ["gpu95", "GPU p95", "мс"],
  ["cpu50", "CPU кадра p50", "мс"], ["cpu95", "CPU p95", "мс"],
];
const n2 = (v) => (v == null ? "—" : Number(v).toLocaleString("ru-RU", { maximumFractionDigits: 2 }));

/** Ряды кадров повтора 1 по пресету: по 100 мс времени маршрута — максимум интервала (видны рывки). */
function series(dir, preset) {
  const f = path.join(dir, `route-${preset}-1.json`);
  if (!fs.existsSync(f)) return null;
  const rec = JSON.parse(fs.readFileSync(f, "utf8"));
  const out = [], marks = [];
  let base = 0;
  for (const s of rec.res?.segs || []) {
    marks.push([base, s.name]);
    const bins = new Map();
    s.frames.forEach((ms, i) => { const b = Math.floor(s.at[i] / 100); bins.set(b, Math.max(bins.get(b) || 0, ms)); });
    for (const [b, v] of [...bins].sort((x, y) => x[0] - y[0])) out.push([base + b / 10, v]);
    base += s.D;
  }
  return { pts: out, marks, total: base };
}

function chart(builds, preset) {
  const data = builds.map((b, i) => ({ b, i, s: b.dir ? series(b.dir, preset) : null })).filter((x) => x.s);
  if (!data.length) return "";
  const W = 1000, H = 260, L = 40, R = 10, T = 12, B = 26, YMAX = 50;
  const total = Math.max(...data.map((d) => d.s.total));
  const x = (t) => L + (t / total) * (W - L - R), y = (ms) => T + (1 - Math.min(ms, YMAX) / YMAX) * (H - T - B);
  const grid = [0, 16.7, 25, 33.3, 50].map((v) => `<line x1="${L}" x2="${W - R}" y1="${y(v)}" y2="${y(v)}" class="grid${v === 16.7 ? " budget" : ""}"/>` +
    `<text x="${L - 6}" y="${y(v) + 4}" class="axis" text-anchor="end">${v}</text>`).join("");
  const segs = data[0].s.marks.map(([t, n], i, a) => `<line x1="${x(t)}" x2="${x(t)}" y1="${T}" y2="${H - B}" class="seg"/>` +
    `<text x="${(x(t) + x(i + 1 < a.length ? a[i + 1][0] : total)) / 2}" y="${H - 8}" class="axis" text-anchor="middle">${esc(SEG_RU[n] || n)}</text>`).join("");
  const lines = data.map((d) => `<polyline class="s${d.i + 1}" points="${d.s.pts.map(([t, v]) => `${x(t).toFixed(1)},${y(v).toFixed(1)}`).join(" ")}"/>`).join("");
  const js = JSON.stringify(data.map((d) => d.s.pts.map(([t, v]) => [Math.round(t * 10) / 10, v])));
  return `<figure class="chart" data-pts='${js}' data-total="${total}" data-l="${L}" data-r="${R}" data-w="${W}" data-names='${esc(JSON.stringify(data.map((d) => d.b.name)))}'>
  <svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Интервал кадра по времени маршрута, ${esc(PRESET_RU[preset])}">${grid}${segs}${lines}
  <line class="cross" y1="${T}" y2="${H - B}" x1="-10" x2="-10"/></svg>
  <figcaption>Худший интервал кадра каждых 100 мс маршрута (повтор 1), мс; срез 50 мс. Пунктир — слот 16,7 мс (60 к/с при делителе 2 на 120 Гц); выше 25 — рывок.</figcaption>
  <div class="tip" hidden></div></figure>`;
}

function verdictCell(r) {
  if (!r) return "";
  if (r.verdict === "=") return `<span class="v eq" title="в пределах шума (порог ${r.noise} %)">≈ ${r.d > 0 ? "+" : ""}${n2(r.d)} %</span>`;
  return `<span class="v ${r.verdict === "хуже" ? "bad" : "good"}">${r.verdict === "хуже" ? "▲ хуже" : "▼ лучше"} ${r.d > 0 ? "+" : ""}${n2(r.d)} %</span>`;
}

export function writePage(tags) {
  const builds = [];
  for (const t of tags) {
    const dir = sessionDir(t);
    const ldir = sessionDir(t, "look");
    if (!dir && !ldir) continue;
    const meta = JSON.parse(fs.readFileSync(path.join(dir || ldir, "meta.json"), "utf8"));
    builds.push({ name: t, dir, ldir, meta, sum: dir ? summarizeSession(dir) : null });
  }
  const base = builds[0];
  const presets = [...new Set(builds.flatMap((b) => Object.keys(b.sum || {})))];
  const cmp = builds.slice(1).map((b) => (base?.sum && b.sum ? compare(base.sum, b.sum) : null));
  const find = (bi, p, seg, k) => cmp[bi - 1]?.rows.find((r) => r.preset === p && r.seg === seg && r.k === k);
  let h = "";

  // Итог по пресетам
  for (const p of presets) {
    h += `<section><h2>Маршрут · ${esc(PRESET_RU[p] || p)}</h2><div class="tablewrap"><table><thead><tr><th>сборка</th>${METRICS.map(([, l]) => `<th>${l}</th>`).join("")}<th>повторов (чистых)</th><th>разброс</th></tr></thead><tbody>`;
    builds.forEach((b, bi) => {
      const s = b.sum?.[p];
      if (!s) return;
      h += `<tr><th scope="row">${esc(b.name)}${bi === 0 && builds.length > 1 ? ' <small>база</small>' : ""}</th>` +
        METRICS.map(([k, , u]) => `<td><b>${n2(s.all[k])}</b>${u ? ` ${u}` : ""}${bi ? `<br>${verdictCell(find(bi, p, "весь маршрут", k))}` : ""}</td>`).join("") +
        `<td>${s.runs} (${s.clean})${s.dirty.length ? `<br><small class="warn" title="${esc(s.dirty.join("; "))}">⚠ грязные: ${s.dirty.length}</small>` : ""}</td>` +
        `<td class="${s.repeat >= 5 ? "warn" : ""}">${n2(s.repeatAll)} % <small>(отрезки до ${n2(s.repeat)} %)</small></td></tr>`;
    });
    h += `</tbody></table></div>${chart(builds, p)}`;
    // Отрезки, причины, проходы — только сборки с маршрутом этого пресета (цвет — по месту сборки в легенде).
    const rb = builds.map((b, i) => ({ ...b, ci: i })).filter((b) => b.sum?.[p]);
    const segNames = Object.keys(builds.find((b) => b.sum?.[p])?.sum[p].segs || {});
    h += `<h3>По отрезкам</h3><div class="tablewrap"><table><thead><tr><th>отрезок</th>${rb.map((b) => `<th colspan="3">${esc(b.name)}</th>`).join("")}</tr>` +
      `<tr><th></th>${rb.map(() => "<th>вовремя</th><th>GPU p50</th><th>CPU p50</th>").join("")}</tr></thead><tbody>`;
    for (const n of segNames) {
      h += `<tr><th scope="row">${esc(SEG_RU[n] || n)}</th>` + rb.map((b) => {
        const g = b.sum?.[p]?.segs[n], bi = b.ci;
        if (!g) return "<td></td><td></td><td></td>";
        return ["share60", "gpu50", "cpu50"].map((k) => `<td>${n2(g[k])}${k === "share60" ? " %" : ""}${bi ? `<br>${verdictCell(find(bi, p, n, k))}` : ""}</td>`).join("");
      }).join("") + "</tr>";
    }
    h += "</tbody></table></div>";
    // Рывки по причинам
    const causes = [...new Set(rb.flatMap((b) => Object.keys(b.sum?.[p]?.causes || {})))];
    if (causes.length) {
      h += `<h3>Рывки > 25 мс по причинам <small>(на прогон; в скобках — из них > 50 мс)</small></h3><div class="tablewrap"><table><thead><tr><th>причина</th>${rb.map((b) => `<th>${esc(b.name)}</th>`).join("")}</tr></thead><tbody>` +
        causes.map((c) => `<tr><th scope="row">${esc(c)}</th>${rb.map((b) => { const v = b.sum?.[p]?.causes[c]; return `<td>${v ? `${n2(v[0])} (${n2(v[1])})` : "—"}</td>`; }).join("")}</tr>`).join("") + "</tbody></table></div>";
    }
    // GPU и CPU
    for (const [kind, key, title] of [["GPU", "gpu", "GPU по проходам (таймеры), мс на кадр, среднее по маршруту"], ["CPU", "sub", "CPU по подсистемам кадра (самописец), мс, среднее по маршруту"]]) {
      const keys = [...new Set(rb.flatMap((b) => Object.keys(b.sum?.[p]?.[key] || {})))].filter((k) => !["post", "total", "cpuTotal"].includes(k));
      const mx = Math.max(0.01, ...rb.flatMap((b) => keys.map((k) => b.sum?.[p]?.[key]?.[k] || 0)));
      h += `<h3>${title}</h3><div class="bars">` + keys.filter((k) => rb.some((b) => (b.sum?.[p]?.[key]?.[k] || 0) >= 0.05)).map((k) =>
        `<div class="barrow"><span class="bl">${esc(k)}</span><span class="bt">${rb.map((b) => { const v = b.sum?.[p]?.[key]?.[k] || 0; return `<span class="bar s${b.ci + 1}" style="width:${(v / mx * 100).toFixed(1)}%" title="${esc(b.name)}: ${n2(v)} мс"></span><span class="bv">${n2(v)}</span>`; }).join("")}</span></div>`).join("") + `</div>`;
      void kind;
    }
    if (cmp.some((c) => c?.where.length)) {
      h += `<h3>Где изменилось (проходы и этапы, > 0,2 мс и 10 %)</h3><ul>` + cmp.flatMap((c, i) => (c?.where || []).filter((w) => w.preset === p).slice(0, 10)
        .map((w) => `<li>${esc(builds[i + 1].name)}: ${w.kind} <b>${esc(w.k)}</b> ${n2(w.a)} → ${n2(w.b)} мс (${w.d > 0 ? "+" : ""}${n2(w.d)})</li>`)).join("") + `</ul>`;
    }
    h += `</section>`;
  }

  // Вид
  const lookBuilds = builds.filter((b) => b.ldir);
  if (lookBuilds.length) {
    const lbase = lookBuilds[0];
    const scenes = [...new Set(lookBuilds.flatMap((b) => fs.readdirSync(path.join(b.ldir, "look")).filter((f) => /^[a-z]+\.json$/.test(f)).map((f) => f.slice(0, -5))))];
    h += `<section><h2>Вид: мерцание и попиксельное сравнение</h2>
    <p class="note">Мерцание — доля пикселей (‰ от пикселей × кадров), чья яркость за два кадра подряд качнулась туда и обратно больше чем на 12 из 255. 60 кадров
    подряд в виртуальном времени (каждый кадр — ровно 1/60 с): две съёмки одной сборки дают то же число. Карта мерцания: красное — пиксели, дрожавшие чаще.
    Сравнение кадров: пиксель изменился, если канал разошёлся больше чем на 16 из 255; маска — красным.</p>
    <div class="tablewrap"><table><thead><tr><th>сцена</th>${lookBuilds.map((b) => `<th>${esc(b.name)}</th>`).join("")}</tr></thead><tbody>`;
    const looks = {};
    for (const n of scenes) {
      h += `<tr><th scope="row">${esc(n)}<br><small>${esc(SCENES[n]?.about || "")}</small></th>`;
      lookBuilds.forEach((b, bi) => {
        const f = path.join(b.ldir, "look", `${n}.json`);
        if (!fs.existsSync(f)) { h += "<td>—</td>"; return; }
        const r = JSON.parse(fs.readFileSync(f, "utf8"));
        (looks[n] ||= [])[bi] = r;
        let extra = "";
        if (bi && looks[n][0]) {
          const d0 = looks[n][0].flicker, d = d0 ? (r.flicker - d0) / d0 * 100 : 0;
          extra = `<br><small>${d > 0 ? "+" : ""}${n2(d)} % к базе</small>`;
        }
        h += `<td><b>${n2(r.flicker)}</b> ‰${extra}</td>`;
      });
      h += "</tr>";
    }
    h += `</tbody></table></div>`;
    for (const n of scenes) {
      const row = lookBuilds.map((b) => ({ b, png: path.join(b.ldir, "look", `${n}.png`), jpg: path.join(b.ldir, "look", `${n}.jpg`), heat: path.join(b.ldir, "look", `${n}-heat.png`) }));
      h += `<h3>${esc(n)}</h3><div class="shots">`;
      row.forEach((x, i) => {
        if (!fs.existsSync(x.jpg)) return;
        h += `<figure><a href="${rel(x.png)}"><img loading="lazy" src="${rel(x.jpg)}" alt="${esc(n)} — ${esc(x.b.name)}"></a><figcaption>${esc(x.b.name)}: кадр 0</figcaption></figure>`;
        h += `<figure><a href="${rel(x.heat)}"><img loading="lazy" src="${rel(x.heat)}" alt="карта мерцания ${esc(n)} — ${esc(x.b.name)}"></a><figcaption>${esc(x.b.name)}: карта мерцания</figcaption></figure>`;
        if (i > 0 && fs.existsSync(row[0].png) && fs.existsSync(x.png)) {
          const out = path.join(x.b.ldir, "look", `${n}-vs-${lbase.name.replace(/[\\/]/g, "_")}.png`);
          let d = null;
          try { d = diffFrames(row[0].png, x.png, out); } catch (e) { d = { error: String(e.message || e) }; }
          h += d.error ? `<p>${esc(d.error)}</p>` : `<figure><a href="${rel(out)}"><img loading="lazy" src="${rel(out)}" alt="маска изменений"></a><figcaption>изменилось ${n2(d.changed)} % пикселей (средняя разница ${n2(d.meanDiff)}) — красным</figcaption></figure>`;
        }
      });
      h += `</div>`;
    }
    h += `</section>`;
  }

  const metaList = builds.map((b) => `<li><b>${esc(b.name)}</b> — ${esc(b.meta.at?.slice(0, 16).replace("T", " "))} UTC, ${esc(b.meta.gpu || "")}${b.meta.extra ? `, адрес + <code>${esc(b.meta.extra)}</code>` : ""}${b.meta.note ? ` — ${esc(b.meta.note)}` : ""}</li>`).join("");
  const html = `<!doctype html><html lang="ru"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>Замеры сборок</title><style>
:root{color-scheme:light;--bg:#fcfcfb;--fg:#0b0b0b;--muted:#52514e;--line:#dddcd7;--grid:#e8e7e3;--s1:#2a78d6;--s2:#eb6834;--s3:#1baf7a;--s4:#eda100;--bad:#c2312f;--good:#1a7f37;--warn:#9a6700}
@media (prefers-color-scheme:dark){:root:not([data-theme="light"]){color-scheme:dark;--bg:#1a1a19;--fg:#f0efec;--muted:#c3c2b7;--line:#3a3a37;--grid:#2c2c2a;--s1:#3987e5;--s2:#d95926;--s3:#199e70;--s4:#c98500;--bad:#ff7b72;--good:#56d364;--warn:#e3b341}}
:root[data-theme="dark"]{color-scheme:dark;--bg:#1a1a19;--fg:#f0efec;--muted:#c3c2b7;--line:#3a3a37;--grid:#2c2c2a;--s1:#3987e5;--s2:#d95926;--s3:#199e70;--s4:#c98500;--bad:#ff7b72;--good:#56d364;--warn:#e3b341}
body{background:var(--bg);color:var(--fg);font:15px/1.45 system-ui,"Segoe UI",sans-serif;margin:0;padding:16px;max-width:1200px;margin-inline:auto}
h1{font-size:24px;margin:8px 0}h2{font-size:19px;margin:28px 0 8px;border-top:1px solid var(--line);padding-top:16px}h3{font-size:16px;margin:18px 0 6px}
small,.note,figcaption{color:var(--muted)}.note{max-width:900px}
.tablewrap{overflow-x:auto}table{border-collapse:collapse;font-variant-numeric:tabular-nums}th,td{padding:6px 10px;border-bottom:1px solid var(--line);text-align:right;vertical-align:top}
th[scope=row],thead th:first-child{text-align:left}thead th{font-weight:600;color:var(--muted);font-size:13px}
.v{font-size:12px;white-space:nowrap}.v.bad{color:var(--bad)}.v.good{color:var(--good)}.v.eq{color:var(--muted)}.warn{color:var(--warn)}
.legend{display:flex;gap:16px;flex-wrap:wrap;margin:8px 0}.legend i{display:inline-block;width:14px;height:3px;border-radius:2px;vertical-align:middle;margin-right:6px}
.chart{margin:12px 0;position:relative}.chart svg{width:100%;height:auto;display:block}
.chart polyline{fill:none;stroke-width:1.5;stroke-linejoin:round}.chart .grid{stroke:var(--grid)}.chart .budget{stroke:var(--muted);stroke-dasharray:4 4}
.chart .seg{stroke:var(--line)}.chart .axis{fill:var(--muted);font-size:11px}.chart .cross{stroke:var(--muted)}
polyline.s1{stroke:var(--s1)}polyline.s2{stroke:var(--s2)}polyline.s3{stroke:var(--s3)}polyline.s4{stroke:var(--s4)}
.bar.s1{background:var(--s1)}.bar.s2{background:var(--s2)}.bar.s3{background:var(--s3)}.bar.s4{background:var(--s4)}
.tip{position:absolute;top:4px;background:var(--bg);border:1px solid var(--line);border-radius:6px;padding:4px 8px;font-size:12px;pointer-events:none;white-space:nowrap}
.bars{display:grid;gap:4px;max-width:900px}.barrow{display:grid;grid-template-columns:110px 1fr;gap:8px;align-items:center}.bl{color:var(--muted);font-size:13px;text-align:right}
.bt{display:grid;grid-template-columns:1fr 52px;gap:2px 6px;align-items:center}.bar{height:8px;border-radius:0 4px 4px 0;min-width:1px}.bv{font-size:12px;color:var(--muted);font-variant-numeric:tabular-nums}
.shots{display:grid;grid-template-columns:repeat(auto-fill,minmax(280px,1fr));gap:10px}.shots figure{margin:0}.shots img{width:100%;border-radius:4px;display:block}
code{font-size:13px}ul{padding-left:20px}
</style></head><body>
<h1>Замеры сборок</h1>
<p class="note">Эталонный маршрут ~85 с на видеокарте ноутбука (RTX 3050, холст 3440×1440, Chrome + ANGLE D3D11), темп 60 к/с (как у пользователя: 120 Гц, делитель 2), авто-качество выключено.
«Вовремя» — интервал ≤ 20,8 мс (кадр попал в свой слот); рывок — интервал > 25 мс. Цена кадра — GPU по таймерам и CPU кадра: они ловят замедление, которое темп ещё прячет.
База — первая сборка; «лучше/хуже» — только если разница больше 5 % и больше полутора разбросов повторов, иначе «≈» (шум). Повторяемость: разброс (макс − мин) / среднее по повторам, цель < 5 %.</p>
<div class="legend">${builds.map((b, i) => `<span><i style="background:var(--s${i + 1})"></i>${esc(b.name)}</span>`).join("")}</div>
<ul>${metaList}</ul>
${h || "<p>Нет замеров.</p>"}
<h2>Как запустить</h2><pre><code>node perf/bench/route.mjs run            # маршрут × 2 пресета × 2 повтора + вид, ~15 мин
node perf/bench/route.mjs compare A B    # сравнение в консоли
node perf/bench/route.mjs page A B       # эта страница</code></pre>
<p class="note">Сделано ${esc(new Date().toISOString().slice(0, 16).replace("T", " "))} UTC · perf/bench/route.mjs · docs/areas/bench.md</p>
<script>
for (const f of document.querySelectorAll(".chart")) {
  const pts = JSON.parse(f.dataset.pts), names = JSON.parse(f.dataset.names), total = +f.dataset.total, L = +f.dataset.l, R = +f.dataset.r, W = +f.dataset.w;
  const svg = f.querySelector("svg"), cross = f.querySelector(".cross"), tip = f.querySelector(".tip");
  svg.addEventListener("mousemove", (e) => {
    const r = svg.getBoundingClientRect(), vx = (e.clientX - r.left) / r.width * W, t = (vx - L) / (W - L - R) * total;
    if (t < 0 || t > total) { tip.hidden = true; return; }
    cross.setAttribute("x1", vx); cross.setAttribute("x2", vx);
    const at = (a) => { let b = a[0]; for (const p of a) if (Math.abs(p[0] - t) < Math.abs(b[0] - t)) b = p; return b; };
    tip.innerHTML = names.map((n, i) => { const p = at(pts[i]); return n + ": <b>" + p[1] + "</b> мс"; }).join("<br>") + "<br>" + t.toFixed(1) + " с";
    tip.hidden = false; tip.style.left = Math.min(r.width - 160, e.clientX - r.left + 12) + "px";
  });
  svg.addEventListener("mouseleave", () => { tip.hidden = true; cross.setAttribute("x1", -10); cross.setAttribute("x2", -10); });
}
</script></body></html>`;
  const out = path.join(BENCH_DIR, "index.html");
  fs.mkdirSync(BENCH_DIR, { recursive: true });
  fs.writeFileSync(out, html);
  return out;
}
