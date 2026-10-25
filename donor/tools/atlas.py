"""Атлас текстур демо: слои 16×16 подряд по вертикали (web/atlas.png) — для TEXTURE_2D_ARRAY.

Источник — пак FamiliarCraft (Scout339, CC BY-SA 4.0) в виде, подготовленном для cubi-core
(textures/familiar-craft: картинки 256×256, рисунки вещей 16×16) и исходные файлы пака.
Порядок слоёв — константы L_* в crates/voxel-core/src/registry/ids.rs, менять вместе (старые номера не переставлять).

Трава, листва и растения — серые: цвет даёт оттенок столбца (Region::tint, как biome blend в Minecraft).
Шейдер: цвет = sRGB→линейный(текстура) × tint², tint = rgb565 / max. Палитра биомов — PALETTE ниже,
её оттенки печатаются для crates/voxel-core/src/gen/mod.rs и запекаются в цветные слои (кайма бока блока 1, горная трава).
Верх травы и листва — свои рисунки (textures/, tools/art_textures.py). Листва чуть окрашена (FOLIAGE): листва
Minecraft темнее и насыщеннее травы того же биома, а оттенок у нас один на столбец.
Бок травы в мире (слои 40, 41) — как в Minecraft «земля + накладка травы»: альфа — маска оттенка (255 — трава,
красится; 128 — земля, не красится; шейдер блоков: mix(1, tint, 2a − 1)). 40 — обычная кайма, 41 — глубокая
кайма ступени в 1 блок (склоны: тонкая тёмная земля в углу вместо коричневых полос).

  python tools/atlas.py <cubi-core/textures/familiar-craft> [<папка textures пака>]

Без папки пака слои из неё (верх песчаника, бок заснеженной травы) берутся из текущего web/atlas.png
по тем же номерам.
"""
import sys
from pathlib import Path

from PIL import Image

sys.dont_write_bytecode = True  # без tools/__pycache__
sys.path.insert(0, str(Path(__file__).resolve().parent))
import art_blocks  # noqa: E402 — блоки срезов 2–5 (свой модуль tools/)

# Средний цвет поверхности травы по биомам (sRGB), мягкий, без кислоты — как Minecraft, чуть приглушённый.
PALETTE = {
    "plains": (88, 122, 46),
    "forest": (70, 110, 40),
    "birch": (86, 124, 54),
    "taiga": (66, 100, 62),
    "snowy": (86, 114, 90),
    "savanna": (134, 126, 58),
    "mountain": (80, 106, 74),
}
# Варианты каймы бока травы (запечённые; ген выбирает ближайший к оттенку столбца). Первый — слой 1.
FRINGES = ["plains", "forest", "taiga", "savanna", ("plains", "savanna"), "mountain"]

# Средняя яркость серых слоёв (sRGB 0..1) и сила контраста рисунка.
GRAY = {
    "grass_top": (0.70, 1.0),
    "grass_full": (0.66, 1.0),
    "short_grass": (0.666, 1.0),  # свои рисунки как есть: тёмные стебли, кончики светлее верха травы
    "fern": (0.688, 1.0),
    "leaves": (0.56, 1.0),
    "birch_leaves": (0.66, 1.0),
    "spruce_leaves": (0.46, 1.0),
}
# Окраска листвы относительно травы (линейная, яркость ≈ 1): по цветам Minecraft — листва дуба/джунглей
# (foliage #77AB2F к траве #91BD59) желтее и насыщеннее, берёза (#80A755) приглушённее, ель (#619961) — в синеву.
FOLIAGE = {
    "leaves": (0.82, 1.1, 0.42),
    "birch_leaves": (0.97, 1.0, 1.06),
    "spruce_leaves": (0.84, 1.06, 0.84),
}
# Свои рисунки (textures/<имя>.png) вместо слоёв пака.
ART = Path(__file__).resolve().parents[1] / "textures"

LAYERS = [
    ("art", "grass_top", "grass_top"), ("fringe", 0), ("tex", "dirt_color"), ("tex", "stone_color"),
    ("tex", "sand_color"), ("tex", "gravel_color"), ("tex", "snow_color"), ("tex", "sandstone_color"),
    ("pack", "mcl_core_sandstone_top"), ("tex", "clay_color"), ("tex", "log_color"), ("tex", "wood_end_color"),
    ("artleaf", "oak_leaves", "leaves"), ("tex", "birch_log_color"), ("tex", "birch_log_end_color"),
    ("artleaf", "birch_leaves", "birch_leaves"),
    ("tex", "spruce_log_color"), ("tex", "spruce_log_end_color"), ("artleaf", "spruce_leaves", "spruce_leaves"),
    ("water", "water_surface"), ("tex", "cactus_color"), ("art", "short_grass", "short_grass"), ("item", "dandelion"),
    ("item", "poppy"), ("item", "dead_bush"), ("tex", "cobblestone_color"), ("tex", "bedrock_color"),
    ("tex", "dry_grass_top_color"), ("tex", "dry_grass_color"), ("tex", "dark_grass_top_color"),
    ("tex", "dark_grass_color"), ("pack", "mcl_core_grass_side_snowed"), ("art", "fern", "fern"),
    # 33: бок травы целиком травой (ступенька в 1 блок, бока дальних плиток) — серый, с оттенком.
    ("art", "grass_top", "grass_full"),
    # 34…38: остальные варианты каймы бока (вариант 0 — слой 1).
    ("fringe", 1), ("fringe", 2), ("fringe", 3), ("fringe", 4), ("fringe", 5),
    # 39: горная трава, запечённая (дальние плитки гор без оттенка: камень на крутых склонах).
    ("baked", "grass_top", "grass_top", "mountain"),
    # 40, 41: бок травы с маской оттенка — обычная кайма и глубокая (ступень в 1 блок).
    ("sidemask", "side"), ("sidemask", "step"),
    # 42…: блоки срезов 2–5 (tools/art_blocks.py, порядок — art_blocks.BLOCKS; константы L_* в crates/voxel-core/src/registry/ids.rs).
    *[("block", name) for name in art_blocks.BLOCKS],
]


def dec(v: float) -> float:
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def enc(v: float) -> float:
    v = max(0.0, min(1.0, v))
    return v * 12.92 if v <= 0.0031308 else 1.055 * v ** (1 / 2.4) - 0.055


def tint_of(name) -> tuple:
    """Оттенок (0..1, уже квантованный в rgb565) для цвета палитры на сером верхе травы."""
    if isinstance(name, tuple):
        col = tuple((x + y) / 2 for x, y in zip(PALETTE[name[0]], PALETTE[name[1]]))
    else:
        col = PALETTE[name]
    g = dec(GRAY["grass_top"][0])
    t = [min(1.0, (dec(c / 255) / g) ** 0.5) for c in col]
    return (round(t[0] * 31) / 31, round(t[1] * 63) / 63, round(t[2] * 31) / 31)


def tile16(im: Image.Image) -> Image.Image:
    im = im.convert("RGBA")
    if im.size == (16, 16):
        return im
    step = im.width // 16
    out = Image.new("RGBA", (16, 16))
    for y in range(16):
        for x in range(16):
            out.putpixel((x, y), im.getpixel((x * step + step // 2, y * step + step // 2)))
    return out


def gray(im: Image.Image, key: str) -> Image.Image:
    """Серый слой: яркость рисунка к заданному среднему; дыркам — средний цвет (без тёмной каймы в mip)."""
    mean, k = GRAY[key]
    px = [im.getpixel((x, y)) for y in range(16) for x in range(16)]
    lum = [(0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]) / 255 for p in px]
    vis = [l for l, p in zip(lum, px) if p[3] >= 128]
    m0 = sum(vis) / len(vis)
    out = Image.new("RGBA", (16, 16))
    hole = int(enc(dec(mean) * 0.55) * 255) if key.endswith("leaves") else int(mean * 255)
    for i, (l, p) in enumerate(zip(lum, px)):
        if p[3] < 128:
            out.putpixel((i % 16, i // 16), (hole, hole, hole, 0))
        else:
            v = int(round(max(0.0, min(1.0, mean + (l - m0) * k)) * 255))
            out.putpixel((i % 16, i // 16), (v, v, v, 255))
    return out


def bake(g: Image.Image, t: tuple) -> Image.Image:
    """Запечь оттенок, как шейдер: линейный(серый) × tint²."""
    out = Image.new("RGBA", (16, 16))
    for y in range(16):
        for x in range(16):
            p = g.getpixel((x, y))
            c = tuple(int(round(enc(dec(p[0] / 255) * t[i] * t[i]) * 255)) for i in range(3))
            out.putpixel((x, y), c + (p[3],))
    return out


# Глубина каймы бока по столбцам пикселей: сверху сплошь трава, ниже — «подтёки», как накладка травы Minecraft
# (3 ряда сплошь, подтёки до 6–7). Ступень в 1 блок — глубокая кайма (9–12): земля видна узкой полосой в углу.
FRINGE_DEPTH = [4, 3, 5, 4, 6, 4, 3, 5, 4, 3, 6, 4, 5, 3, 4, 5]
STEP_DEPTH = [10, 9, 11, 10, 12, 10, 9, 11, 10, 9, 12, 10, 11, 9, 10, 11]


def art(name: str) -> Image.Image:
    return Image.open(ART / f"{name}.png").convert("RGBA")


def dirt_side(tex: Path) -> Image.Image:
    return tile16(Image.open(tex / "dirt_color.png"))


def fringe_layer(tex: Path, depth: list, grass, mask: bool) -> Image.Image:
    """Бок травы: сверху трава (серая с маской или запечённая), под каймой тень, ниже земля."""
    dirt = dirt_side(tex)
    out = Image.new("RGBA", (16, 16))
    for x in range(16):
        d = depth[x]
        for y in range(16):
            if y < d:
                p = grass.getpixel((x, y))
                out.putpixel((x, y), (p[0], p[1], p[2], 255))
                continue
            p = dirt.getpixel((x, y))
            k = 0.74 if y == d else 1.0  # тень под каймой
            out.putpixel((x, y), (int(p[0] * k), int(p[1] * k), int(p[2] * k), 128 if mask else 255))
    return out


def fringe(tex: Path, t: tuple) -> Image.Image:
    return fringe_layer(tex, FRINGE_DEPTH, bake(gray(art("grass_top"), "grass_top"), t), False)


def foliage(im: Image.Image, key: str) -> Image.Image:
    f = FOLIAGE[key]
    out = Image.new("RGBA", (16, 16))
    for y in range(16):
        for x in range(16):
            p = im.getpixel((x, y))
            out.putpixel((x, y), tuple(int(round(enc(dec(p[i] / 255) * f[i]) * 255)) for i in range(3)) + (p[3],))
    return out


def main() -> None:
    tex = Path(sys.argv[1])
    pack = Path(sys.argv[2]) if len(sys.argv) > 2 else None
    out_path = Path(__file__).resolve().parents[1] / "web" / "atlas.png"
    old = Image.open(out_path).convert("RGBA") if out_path.exists() else None
    atlas = Image.new("RGBA", (16, 16 * len(LAYERS)))
    blocks = art_blocks.build(tex)
    for i, spec in enumerate(LAYERS):
        kind = spec[0]
        if kind == "gray":
            im = gray(tile16(Image.open(tex / f"{spec[1]}.png")), spec[2])
        elif kind == "art":
            im = gray(art(spec[1]), spec[2])
        elif kind == "artleaf":
            im = foliage(gray(art(spec[1]), spec[2]), spec[2])
        elif kind == "block":
            im = blocks[spec[1]]
        elif kind == "sidemask":
            im = fringe_layer(tex, FRINGE_DEPTH if spec[1] == "side" else STEP_DEPTH, gray(art("grass_top"), "grass_top"), True)
        elif kind == "grayitem":
            im = gray(tile16(Image.open(tex / "items" / f"{spec[1]}.png")), spec[2])
        elif kind == "fringe":
            im = fringe(tex, tint_of(FRINGES[spec[1]]))
        elif kind == "baked":
            im = bake(gray(art(spec[1]), spec[2]), tint_of(spec[3]))
        elif kind in ("tex", "water"):
            im = tile16(Image.open(tex / f"{spec[1]}.png"))
        elif kind == "item":
            im = tile16(Image.open(tex / "items" / f"{spec[1]}.png"))
        else:
            # Слой из папки пака (кадр 16×16) или, без неё, из текущего атласа по тому же номеру.
            if pack is not None:
                im = Image.open(pack / f"{spec[1]}.png").convert("RGBA").crop((0, 0, 16, 16))
            else:
                im = old.crop((0, 16 * i, 16, 16 * i + 16))
        atlas.paste(im, (0, 16 * i))
    atlas.save(out_path)
    print(f"{len(LAYERS)} layers -> {out_path}")
    # Оттенки палитры для crates/voxel-core/src/gen/mod.rs (tools/world_palette.py вписывает их сам).
    for name in PALETTE:
        t = tint_of(name)
        print(f"const T_{name.upper()}: [f32; 3] = [{t[0]:.4f}, {t[1]:.4f}, {t[2]:.4f}];")
    print("const FRINGE_TINTS: [[f32; 3]; 6] = [" + ", ".join(
        "[{:.4f}, {:.4f}, {:.4f}]".format(*tint_of(f)) for f in FRINGES) + "];")


if __name__ == "__main__":
    main()
