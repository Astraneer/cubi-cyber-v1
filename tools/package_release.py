#!/usr/bin/env python3
"""Собрать переносимый ZIP из явно выбранной сборки, с хешами и уведомлениями."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parents[1]
FIXED_TIME = (2026, 1, 1, 0, 0, 0)


def local(path):
    path = Path(path).resolve()
    if not path.is_relative_to(ROOT):
        raise ValueError("Путь должен находиться внутри репозитория")
    return path


def file_bytes(path):
    path = local(path)
    if not path.is_file():
        raise ValueError(f"Нет входного файла: {path.relative_to(ROOT)}")
    return path.read_bytes()


def legal_files(metadata):
    files = {}
    inventory = []
    for name in ("LICENSE-MIT", "LICENSE-APACHE", "ASSET_CREDITS.md"):
        files[name] = file_bytes(ROOT / name)
    files["licenses/assets/sounds-CREDITS.md"] = file_bytes(ROOT / "donor/web/sounds/CREDITS.md")
    files["licenses/assets/atlas-provenance.py"] = file_bytes(ROOT / "donor/tools/atlas.py")
    files["licenses/assets/IMPORT.json"] = file_bytes(ROOT / "donor/IMPORT.json")
    for name in ("LICENSE-NVIDIA", "PROVENANCE.json"):
        files["licenses/crates/nv-flip-sys-0.1.0/" + name] = file_bytes(ROOT / "crates/compat-nv-flip-sys" / name)
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["source"] is None:
            continue
        directory = local(Path(package["manifest_path"]).parent)
        prefix = f"licenses/crates/{package['name']}-{package['version']}"
        paths = [p for p in directory.iterdir() if p.is_file() and p.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "AUTHORS"))]
        if package.get("license_file"):
            paths.append(directory / package["license_file"])
        entries = set()
        for path in paths:
            path = local(path)
            if not path.is_relative_to(directory):
                raise ValueError("Уведомление пакета выходит за его каталог")
            name = prefix + "/" + path.relative_to(directory).as_posix()
            files[name] = file_bytes(path)
            entries.add(name)
        # Some published packages omit a separate license file. Preserve their
        # complete original registry source archive, metadata and README instead
        # of inventing copyright or fetching another repository.
        if not entries:
            registry = directory.parent.name
            archive = directory.parents[2] / "cache" / registry / f"{directory.name}.crate"
            name = prefix + "/" + archive.name
            files[name] = file_bytes(archive)
            entries.add(name)
            for filename in ("Cargo.toml", "Cargo.toml.orig", "README.md"):
                path = directory / filename
                if path.is_file():
                    files[prefix + "/" + filename] = file_bytes(path)
        inventory.append({"name": package["name"], "version": package["version"],
                          "license": package["license"], "authors": package["authors"],
                          "repository": package["repository"], "evidence": sorted(entries)})
    files["licenses/crates.json"] = (json.dumps(inventory, ensure_ascii=False, indent=2) + "\n").encode()
    return files


def payload(kind, directory, platform):
    directory = local(directory)
    if kind == "web":
        names = ("index.html", "pkg/client_web.js", "pkg/client_web_bg.wasm", "workers/jobs.js", "workers/scene.js")
        files = {name: file_bytes(directory / name) for name in names}
        if not files["pkg/client_web_bg.wasm"].startswith(b"\0asm"):
            raise ValueError("Входной WASM не является модулем WebAssembly")
        files["serve_web.py"] = file_bytes(ROOT / "tools/serve_web.py")
        files["ЗАПУСК.txt"] = ("Python 3: python serve_web.py --directory .\n"
                                "Открыть http://127.0.0.1:8000 в браузере с WebGPU.\n"
                                "Сервер игры запускается отдельно. Публикация на VPS не выполнялась.\n").encode()
        return files
    extension = ".exe" if platform == "windows" else ""
    names = ("client-native" + extension, "server" + extension)
    files = {name: file_bytes(directory / name) for name in names}
    files["benchmark" + extension] = file_bytes(directory / "examples" / ("benchmark" + extension))
    files["movement-replay" + extension] = file_bytes(directory / "examples" / ("replay" + extension))
    signature = b"MZ" if platform == "windows" else b"\x7fELF"
    if any(not value.startswith(signature) for value in files.values()):
        raise ValueError("Формат программы не соответствует выбранной платформе")
    files["bench_report.py"] = file_bytes(ROOT / "tools/bench_report.py")
    prefix = ".\\" if platform == "windows" else "./"
    shell = "PowerShell" if platform == "windows" else "терминал"
    files["ЗАПУСК.txt"] = (
        f"Полностью распакуйте ZIP и откройте {shell} в его каталоге.\n"
        "Для создания HTML нужен Python 3; Rust и исходники не нужны.\n"
        "Ограничения этой сборки: docs/STATUS.md. Подробности измерений: docs/M13.md.\n"
        "\nИгра: сервер и клиент запускаются в отдельных окнах терминала.\n"
        f"{prefix}server{extension} --data saves/world\n"
        f"{prefix}client-native{extension} --connect ws://127.0.0.1:25565\n"
        "\nОконный маршрут для измерения на вашей видеокарте:\n"
        f"{prefix}client-native{extension} --benchmark measurements/route-a.json\n"
        "Он сам завершится после 1800 кадров. Это отдельный лесной маршрут;\n"
        "не добавляйте --connect или --smoke-frames и не меняйте размер окна во время прогона.\n"
        "python bench_report.py report measurements/route-a.json --out measurements/route-a.html\n"
        "\nПовтор B: сохраните то же разрешение, backend/GPU и сравнимые настройки.\n"
        f"{prefix}client-native{extension} --benchmark measurements/route-b.json\n"
        "python bench_report.py report measurements/route-b.json --out measurements/route-b.html\n"
        "python bench_report.py compare measurements/route-a.json measurements/route-b.json --out measurements/compare.html\n"
        "Несовместимые маршрут, размер или область измерения будут отклонены.\n"
        "\nF8 в игре: после загрузки дайте записи накопить не менее 10 секунд и нажмите F8.\n"
        "JSON находится в .local/artifacts/flight/native-*.json. Скопируйте нужную запись в F8.json:\n"
        "python bench_report.py report F8.json --out measurements/f8.html\n"
        f"{prefix}movement-replay{extension} F8.json\n"
        "\nДополнительная проверка без окна (отдельная область измерения):\n"
        f"{prefix}benchmark{extension} --out measurements/offscreen.json\n"
        "Не сравнивайте её с оконным маршрутом: она ожидает GPU между кадрами.\n"
        "\nЧто прислать: route-a.json и route-a.html; для A/B также route-b.json,\n"
        "route-b.html и compare.html; для игрового рывка — F8.json и f8.html; BUILD.json.\n"
        "Укажите backend из JSON, модель GPU, версию драйвера, режим питания, разрешение,\n"
        "рендер-масштаб, MSAA, дальность и включённые эффекты; опишите, когда был рывок.\n"
        "Не присылайте saves/, identity, .local/client/, .minecraft, ресурс-паки\n"
        "или пути к личным ресурсам. Эти файлы не нужны для отчёта.\n"
        "Измерения сохраняются локально и автоматически никуда не отправляются.\n"
    ).encode()
    return files


def archive(out, files, provenance):
    out = local(out)
    for name in files:
        path = Path(name)
        if path.is_absolute() or ".." in path.parts or "\\" in name:
            raise ValueError("Небезопасное имя в архиве")
    hashes = {name: hashlib.sha256(value).hexdigest() for name, value in sorted(files.items())}
    files = {**files, "BUILD.json": (json.dumps({**provenance, "sha256": hashes}, ensure_ascii=False, indent=2) + "\n").encode()}
    out.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as bundle:
        for name, data in sorted(files.items()):
            info = zipfile.ZipInfo(name, date_time=FIXED_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            mode = 0o755 if name in ("client-native", "server", "benchmark", "movement-replay") else 0o644
            info.external_attr = (0o100000 | mode) << 16
            bundle.writestr(info, data)
    checksum = hashlib.sha256(out.read_bytes()).hexdigest()
    out.with_suffix(out.suffix + ".sha256").write_text(f"{checksum}  {out.name}\n", encoding="utf-8")
    return len(files), checksum


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("native", "web"))
    parser.add_argument("--directory", type=Path, required=True)
    parser.add_argument("--platform", choices=("linux", "windows"), default="linux")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    # Includes only locked package metadata; does not fetch or execute dependencies.
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--offline", "--format-version=1"], cwd=ROOT))
    files = payload(args.kind, args.directory, args.platform)
    files.update(legal_files(metadata))
    for name in ("STATUS.md", "M13.md"):
        files["docs/" + name] = file_bytes(ROOT / "docs" / name)
    sha = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT))
    count, digest = archive(args.out, files, {"schemaVersion": 1, "sourceCommit": sha, "sourceDirty": dirty, "kind": args.kind, "platform": args.platform if args.kind == "native" else "web"})
    print(f"PACKAGE_OK files={count} sha256={digest} output={args.out}")


if __name__ == "__main__":
    main()
