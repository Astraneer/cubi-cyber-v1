#!/usr/bin/env python3
"""Локальный HTML-отчёт и сравнение фактических записей telemetry::flight."""
import argparse
import html
import json
import math
from pathlib import Path

MAX_BYTES = 32 * 1024 * 1024
PASS_NAMES = ("frustum", "depth", "hiz", "occlusion", "shadows", "sky", "terrain",
              "cutout", "water", "clouds", "gi", "exposure", "bloom", "tonemap", "models", "ui")
OBSERVATIONS = ("Аллокация", "Компиляция", "Синхронная генерация", "Бюджет загрузки",
                "Возраст задания", "Ожидание GPU", "Бюджет света", "Ожидание записи")


def integer(value, name, maximum=(1 << 64) - 1):
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError(f"Некорректное поле {name}")
    return value


def load(path):
    path = Path(path)
    if path.stat().st_size > MAX_BYTES:
        raise ValueError("Запись превышает 32 МиБ")
    def reject_constant(_):
        raise ValueError("NaN/Infinity в записи")
    value = json.loads(path.read_text(encoding="utf-8"), parse_constant=reject_constant)
    validate(value)
    return value


def validate(value):
    if not isinstance(value, dict) or type(value.get("schemaVersion")) is not int or value["schemaVersion"] != 1:
        raise ValueError("Неизвестная версия записи")
    if not isinstance(value.get("backend"), str) or len(value["backend"]) > 1024:
        raise ValueError("Неизвестный backend")
    frames = value.get("frames")
    if not isinstance(frames, list) or not 1 <= len(frames) <= 65536:
        raise ValueError("Нет кадров или превышен лимит")
    for key in ("lostFrames", "lostInputs", "windowNs"):
        integer(value.get(key), key)
    if type(value.get("completeWindow")) is not bool:
        raise ValueError("Не указан статус временного окна")
    last_time = last_frame = -1
    for frame in frames:
        if not isinstance(frame, dict):
            raise ValueError("Некорректный кадр")
        for key in ("frame", "timeNs", "intervalNs", "observations", "queuedJobs", "oldestJobTicks"):
            integer(frame.get(key), key)
        if frame["timeNs"] < last_time or frame["frame"] <= last_frame:
            raise ValueError("Нарушен порядок кадров")
        last_time, last_frame = frame["timeNs"], frame["frame"]
        integer(frame.get("inFlight"), "inFlight", 2)
        for key in ("cpuNs", "gpuNs"):
            if frame.get(key) is not None:
                integer(frame[key], key)
        for key, count in (("cpu", 8), ("gpu", 16)):
            times = frame.get(key)
            if not isinstance(times, list) or len(times) != count:
                raise ValueError("Несовместимая таблица проходов")
            for timing in times:
                if timing is not None:
                    integer(timing, key)
        work = frame.get("work")
        if not isinstance(work, dict):
            raise ValueError("Нет счётчиков работы")
        for key in ("cells", "quads", "uploads", "pipelines", "syncGeneration"):
            integer(work.get(key), key)
        allocation = frame.get("allocations")
        if allocation is not None:
            if not isinstance(allocation, dict):
                raise ValueError("Некорректные счётчики аллокатора")
            for key in ("alloc", "realloc", "dealloc", "allocatedBytes"):
                integer(allocation.get(key), key)


def percentile(values, fraction):
    if not values:
        return None
    values = sorted(values)
    return values[max(0, math.ceil(len(values) * fraction) - 1)]


def distribution(values):
    known = [v / 1_000_000 for v in values if v is not None]
    return {"samples": len(known), "p50": percentile(known, .5),
            "p95": percentile(known, .95), "p99": percentile(known, .99)}


def summarize(value):
    validate(value)
    frames = value["frames"]
    work = {key: sum(f["work"][key] for f in frames)
            for key in ("cells", "quads", "uploads", "pipelines", "syncGeneration")}
    passes = []
    names = value.get("gpuPasses", PASS_NAMES)
    if not isinstance(names, list) or len(names) != 16 or not all(isinstance(n, str) for n in names):
        names = PASS_NAMES
    for index, name in enumerate(names):
        passes.append({"name": name, **distribution([f["gpu"][index] for f in frames])})
    stage_names = value.get("cpuStages", ("input", "simulation", "jobs", "mesh", "upload", "encode", "present", "other"))
    if not isinstance(stage_names, (tuple, list)) or len(stage_names) != 8 or not all(isinstance(n, str) for n in stage_names):
        raise ValueError("Несовместимая таблица подсистем CPU")
    stages = [{"name": name, **distribution([f["cpu"][index] for f in frames])}
              for index, name in enumerate(stage_names)]
    return {"backend": value["backend"], "frames": len(frames),
            "durationSeconds": (frames[-1]["timeNs"] - frames[0]["timeNs"]) / 1e9,
            "completeWindow": value["completeWindow"], "lostFrames": value["lostFrames"],
            "lostInputs": value["lostInputs"], "benchmark": value.get("benchmark"),
            "frame": distribution([f["intervalNs"] or None for f in frames]),
            "cpu": distribution([f.get("cpuNs") for f in frames]),
            "gpu": distribution([f.get("gpuNs") for f in frames]), "work": work,
            "passes": passes, "stages": stages,
            "observations": {name: sum(bool(f["observations"] & (1 << bit)) for f in frames)
                             for bit, name in enumerate(OBSERVATIONS)},
            "allocationSamples": sum(f["allocations"] is not None for f in frames),
            "allocCalls": sum(f["allocations"]["alloc"] + f["allocations"]["realloc"]
                              for f in frames if f["allocations"] is not None),
            "timeline": [[f["timeNs"] / 1e6, f["intervalNs"] / 1e6,
                          None if f.get("cpuNs") is None else f["cpuNs"] / 1e6,
                          None if f.get("gpuNs") is None else f["gpuNs"] / 1e6] for f in frames]}


def fmt(value):
    return "не измерено" if value is None else f"{value:.3f}"


def compatible(a, b):
    if a["backend"] != b["backend"] or a.get("publicSeed") != b.get("publicSeed"):
        return False
    if len(a["frames"]) != len(b["frames"]) or a.get("initialHash") != b.get("initialHash"):
        return False
    left, right = a.get("benchmark"), b.get("benchmark")
    if not isinstance(left, dict) or not isinstance(right, dict):
        return False
    return all(left.get(k) is not None and left[k] == right.get(k)
               for k in ("route", "width", "height", "presentationIncluded")) and all(
                   left.get(k) == right.get(k) for k in ("blockingReadback", "framesRequested", "scene"))


def render(records):
    summaries = [summarize(record) for record in records]
    sections = []
    for index, summary in enumerate(summaries):
        metadata = summary["benchmark"]
        details = ""
        if isinstance(metadata, dict):
            details = (f"<p>Маршрут: {html.escape(str(metadata.get('route', 'не указан')))}; "
                       f"размер: {html.escape(str(metadata.get('width', '?')))} × {html.escape(str(metadata.get('height', '?')))}. "
                       f"Вывод в окно {'включён' if metadata.get('presentationIncluded') else 'не включён'}; "
                       f"полный маршрут: {'да' if metadata.get('routeComplete') else 'не подтверждён'}.</p>")
            if metadata.get("blockingReadback"):
                details += "<p>Этот прогон ожидает завершения GPU между кадрами; длительность включает ожидание readback.</p>"
            if metadata.get("colorPassIncludesSky"):
                details += "<p>Проход terrain объединяет небо и геометрию; отдельное время неба не измерено.</p>"
        rows = "".join(f"<tr><td>{label}</td><td>{fmt(summary[key]['p50'])}</td>"
                       f"<td>{fmt(summary[key]['p95'])}</td><td>{fmt(summary[key]['p99'])}</td>"
                       f"<td>{summary[key]['samples']}</td></tr>"
                       for label, key in (("Кадр", "frame"), ("CPU", "cpu"), ("GPU", "gpu")))
        passes = "".join(f"<tr><td>{html.escape(p['name'])}</td><td>{fmt(p['p50'])}</td>"
                         f"<td>{fmt(p['p95'])}</td><td>{fmt(p['p99'])}</td><td>{p['samples']}</td></tr>"
                         for p in summary["passes"])
        stages = "".join(f"<tr><td>{html.escape(p['name'])}</td><td>{fmt(p['p50'])}</td>"
                         f"<td>{fmt(p['p95'])}</td><td>{fmt(p['p99'])}</td><td>{p['samples']}</td></tr>"
                         for p in summary["stages"])
        work = html.escape(json.dumps(summary["work"], ensure_ascii=False))
        observed = " · ".join(f"{html.escape(k)}: {v}" for k, v in summary["observations"].items() if v)
        sections.append(f"""<section><h2>{'AB'[index]} · {html.escape(summary['backend'])}</h2>
{details}<p>{summary['frames']} кадров · {summary['durationSeconds']:.2f} с ·
окно {'полное' if summary['completeWindow'] else 'неполное'} ·
потери кадров {summary['lostFrames']}, ввода {summary['lostInputs']}</p>
<table><thead><tr><th>Измерение, мс</th><th>p50</th><th>p95</th><th>p99</th><th>Образцов</th></tr></thead><tbody>{rows}</tbody></table>
<p>Аллокатор: {summary['allocationSamples']} измеренных кадров, {summary['allocCalls']} вызовов.</p>
<p>Сопутствующие события: {observed or 'не зарегистрированы'}. Причинность по этим флагам не устанавливается.</p>
<p><code>{work}</code></p><svg class="plot" data-series="{index}" viewBox="0 0 1000 240" role="img" aria-label="Время кадров"><text x="8" y="20">График загружается</text></svg>
<p class="hover" id="hover{index}">Наведите на график: кадр · CPU · GPU, мс.</p>
<details><summary>Подсистемы CPU</summary><table><thead><tr><th>Подсистема</th><th>p50</th><th>p95</th><th>p99</th><th>Образцов</th></tr></thead><tbody>{stages}</tbody></table></details>
<details><summary>Проходы GPU</summary><table><thead><tr><th>Проход</th><th>p50</th><th>p95</th><th>p99</th><th>Образцов</th></tr></thead><tbody>{passes}</tbody></table></details></section>""")
    comparison = ""
    if len(records) == 2:
        if compatible(*records):
            before, after = summaries[0]["frame"]["p95"], summaries[1]["frame"]["p95"]
            delta = None if not before or after is None else (after / before - 1) * 100
            comparison = f"<p>Сравнение p95 B/A: {fmt(delta)} %; отрицательное значение — быстрее.</p>"
        else:
            comparison = "<p>Сопоставимость не подтверждена: различаются backend/маршрут/размер/seed либо нет метаданных маршрута.</p>"
    payload = json.dumps(summaries, ensure_ascii=False, allow_nan=False).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
    return """<!doctype html><html lang="ru"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Cubi Cyber · измерения</title>
<style>body{margin:2rem auto;max-width:1100px;padding:0 1rem;background:#171d21;color:#e6e9e4;font:16px system-ui}h1,h2{color:#bce0ad}section{border:1px solid #52635d;padding:1rem;margin:1rem 0}table{border-collapse:collapse;width:100%}td,th{padding:.4rem;text-align:right;border-bottom:1px solid #34453d}td:first-child,th:first-child{text-align:left}.plot{width:100%;background:#11191e}code{white-space:pre-wrap}.hover{font-variant-numeric:tabular-nums}summary{cursor:pointer}</style>
<h1>Cubi Cyber · измерения</h1><p>Показаны фактические записи. Неизвестные времена сохранены как «не измерено»; программный адаптер не подтверждает бюджет RTX. Время кадра и CPU-кодирования — разные измерения.</p>""" + comparison + "".join(sections) + f"<script type=\"application/json\" id=\"data\">{payload}</script>" + """<script>
const data=JSON.parse(document.getElementById('data').textContent),ns='http://www.w3.org/2000/svg';
document.querySelectorAll('.plot').forEach(svg=>{const index=Number(svg.dataset.series),rows=data[index].timeline;svg.textContent='';const max=rows.reduce((m,r)=>Math.max(m,...r.slice(1).filter(v=>v!==null)),16.667)*1.05;
for(const [column,color]of [[1,'#bce0ad'],[2,'#eabf7a'],[3,'#91bce0']]){let parts=[],part=[];for(let i=0;i<rows.length;i++){const v=rows[i][column];if(v===null){if(part.length)parts.push(part.join(' '));part=[];continue;}part.push(`${i/Math.max(1,rows.length-1)*1000},${235-v/max*220}`);}if(part.length)parts.push(part.join(' '));for(const points of parts){const line=document.createElementNS(ns,'polyline');line.setAttribute('points',points);line.setAttribute('fill','none');line.setAttribute('stroke',color);svg.appendChild(line);}}
const budget=document.createElementNS(ns,'line');budget.setAttribute('x2','1000');budget.setAttribute('y1',235-16.667/max*220);budget.setAttribute('y2',235-16.667/max*220);budget.setAttribute('stroke','#6d767b');budget.setAttribute('stroke-dasharray','5 5');svg.appendChild(budget);
svg.onpointermove=e=>{const bounds=svg.getBoundingClientRect(),i=Math.max(0,Math.min(rows.length-1,Math.round((e.clientX-bounds.left)/bounds.width*(rows.length-1))));document.getElementById('hover'+index).textContent=`Кадр ${i+1}: `+rows[i].slice(1).map(v=>v===null?'не измерено':v.toFixed(3)).join(' · ');};});
</script></html>"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("report", "compare"))
    parser.add_argument("records", nargs="+", type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if len(args.records) != (1 if args.command == "report" else 2):
        parser.error("report требует одну запись, compare — две")
    records = [load(path) for path in args.records]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(render(records), encoding="utf-8")
    print(f"BENCH_REPORT_OK frames={','.join(str(len(r['frames'])) for r in records)} output={args.out}")


if __name__ == "__main__":
    main()
