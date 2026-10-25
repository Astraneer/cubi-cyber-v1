"""Текстуры мобов (свои, не Mojang): web/mobs.png — один лист 128×128 с развёртками коробок всех мобов
и web/mob_uv.js — где лежит каждая коробка (u, v, ш, в, г в пикселях листа), оттуда её читает web/mobs.js.

Стиль мира (пак FamiliarCraft, CC BY-SA 4.0, как блоки и предметы): 16 пикселей на блок, рампа из пяти тонов (обводка/тень,
тень, середина, свет, блик), чистые кластеры без размытия, светотень лёгкая — свет считает шейдер. Пропорции частей —
ванильные для мобов Minecraft (размеры коробок — модельные пиксели в web/mobs.js), а рисунок и раскладка листа — свои.

Развёртка коробки ш×в×г с началом (u, v): верхний ряд высотой г — [г пусто][верх −Y: ш][низ +Y: ш]; нижний ряд высотой в —
[бок −X: г][перёд −Z: ш][бок +X: г][зад +Z: ш]. Грани нарисованы так, как их видно снаружи (перёд моба — −Z модели; у
квадрупедов и курицы тело повёрнуто на π/2: +Z модели — спина, −Z — брюхо, −Y — шея, +Y — круп). Рисунок считается по
трёхмерной точке на поверхности коробки (шум, пятна), поэтому через рёбра он не рвётся.

  python -I tools/mob_art.py [png-превью-в-4×]
"""
import math
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SIZE = 128

# ---------------------------------------------------------------------------------------------------------------- рампы
PINK = [(160, 80, 90), (220, 132, 138), (240, 160, 160), (252, 188, 182), (255, 214, 204)]
COW_BROWN = [(50, 32, 24), (88, 58, 40), (116, 78, 52), (142, 100, 66), (168, 126, 88)]
COW_WHITE = [(168, 160, 152), (208, 202, 194), (230, 226, 218), (245, 242, 236), (255, 253, 250)]
CREAM = [(150, 116, 96), (196, 160, 134), (216, 186, 160), (232, 206, 182), (244, 226, 204)]
SHEEP_FACE = [(52, 42, 42), (82, 68, 64), (106, 90, 82), (130, 112, 102), (154, 134, 122)]
SHEEP_MUZZLE = [(120, 92, 86), (170, 140, 128), (196, 166, 152), (214, 186, 172), (230, 204, 190)]
SHEEP_LEG = [(60, 44, 40), (98, 80, 72), (124, 102, 92), (150, 126, 112), (176, 150, 134)]
WOOL = [(176, 176, 182), (204, 204, 210), (226, 226, 230), (242, 242, 245), (255, 255, 255)]
FEATH = [(146, 150, 166), (198, 202, 214), (226, 229, 238), (246, 247, 251), (255, 255, 255)]
YELLOW = [(150, 88, 20), (212, 134, 32), (242, 168, 54), (252, 198, 94), (255, 224, 142)]
RED = [(110, 18, 20), (168, 32, 30), (206, 48, 40), (230, 78, 62), (248, 120, 96)]
ZSKIN = [(40, 78, 38), (64, 112, 54), (88, 142, 72), (114, 168, 92), (142, 192, 116)]
SHIRT = [(16, 80, 88), (28, 118, 128), (42, 152, 158), (60, 180, 182), (92, 204, 200)]
PANTS = [(22, 20, 62), (40, 36, 104), (60, 54, 140), (82, 76, 166), (108, 102, 190)]
SHOE = [(28, 26, 32), (44, 42, 50), (62, 58, 68), (82, 78, 90), (104, 100, 114)]
DARK = (22, 18, 22)


# ------------------------------------------------------------------------------------------------------------------ шум
def hsh(ix, iy, iz, k=0):
    """Детерминированный шум 0…1 по целой точке."""
    n = (ix * 73856093) ^ (iy * 19349663) ^ (iz * 83492791) ^ (k * 2654435761)
    n = (n ^ (n >> 13)) * 1274126177 & 0xFFFFFFFF
    return ((n ^ (n >> 16)) & 0xFFFF) / 65535.0


def smooth(t):
    return t * t * (3 - 2 * t)


def vnoise(x, y, z, k=0):
    """Гладкий шум по решётке (трилинейно), 0…1."""
    ix, iy, iz = math.floor(x), math.floor(y), math.floor(z)
    fx, fy, fz = smooth(x - ix), smooth(y - iy), smooth(z - iz)
    out = 0.0
    for dx in (0, 1):
        for dy in (0, 1):
            for dz in (0, 1):
                w = (fx if dx else 1 - fx) * (fy if dy else 1 - fy) * (fz if dz else 1 - fz)
                out += w * hsh(ix + dx, iy + dy, iz + dz, k)
    return out


def fbm(x, y, z, k=0):
    return vnoise(x, y, z, k) * 0.65 + vnoise(x * 2.1, y * 2.1, z * 2.1, k + 7) * 0.35


def pick(ramp, t):
    """Тон рампы по вещественному t (0…4), с округлением к ближайшему."""
    return ramp[max(0, min(4, int(t + 0.5)))]


def speck(ramp, base, X, Y, Z, k=0, amp=1.0, scale=1.0):
    """Тон base (0…4) с кластерным шумом ±amp."""
    n = fbm(X * scale * 0.6, Y * scale * 0.6, Z * scale * 0.6, k)
    t = base + (n - 0.5) * 2 * amp * 1.5
    return pick(ramp, t)


# ---------------------------------------------------------------------------------------------------------------- точка
class Px:
    """Пиксель грани: грань, координаты в грани, точка на поверхности коробки (модельные пиксели от угла −X −Y −Z)."""

    __slots__ = ("face", "x", "y", "fw", "fh", "X", "Y", "Z", "w", "h", "d", "salt")

    def front_dist(self):
        """Боковая грань: расстояние в пикселях от переднего ребра (0 — у перед)."""
        return (self.fw - 1 - self.x) if self.face == "west" else self.x

    def lit(self):
        """Свет от верха-слева для светотени по грани: top 1, боковые 0, низ −1 (лёгкий, остальное сделает шейдер)."""
        return {"top": 1, "bottom": -1}.get(self.face, 0)


def point(face, x, y, w, h, d):
    p = Px()
    p.face, p.x, p.y, p.w, p.h, p.d = face, x, y, w, h, d
    cx, cy = x + 0.5, y + 0.5
    if face == "front":
        p.fw, p.fh, p.X, p.Y, p.Z = w, h, cx, cy, 0
    elif face == "back":
        p.fw, p.fh, p.X, p.Y, p.Z = w, h, w - cx, cy, d
    elif face == "west":
        p.fw, p.fh, p.X, p.Y, p.Z = d, h, 0, cy, d - cx
    elif face == "east":
        p.fw, p.fh, p.X, p.Y, p.Z = d, h, w, cy, cx
    elif face == "top":
        p.fw, p.fh, p.X, p.Y, p.Z = w, d, w - cx, 0, cy
    else:  # bottom
        p.fw, p.fh, p.X, p.Y, p.Z = w, d, cx, h, cy
    return p


def faces_of(u, v, w, h, d):
    """Прямоугольники граней коробки на листе: имя, u, v, ширина, высота."""
    return [
        ("top", u + d, v, w, d), ("bottom", u + d + w, v, w, d),
        ("west", u, v + d, d, h), ("front", u + d, v + d, w, h), ("east", u + d + w, v + d, d, h), ("back", u + d + w + d, v + d, w, h),
    ]


# ------------------------------------------------------------------------------------------------------------- рисунки
def side_shade(p, ramp, base):
    """Лёгкая светотень по грани: верх светлее, низ темнее."""
    return max(0, min(4, base + p.lit()))


def pig_skin(p, k=0, belly_axis="y"):
    """Кожа свиньи: мягкие пятна, спина светлее брюха, редкие тёмные веснушки."""
    # вертикаль: у головы и ног — Y, у тела (лежит) — Z (брюхо −Z, спина +Z)
    up = (p.Z / p.d) if belly_axis == "z" else 1 - p.Y / p.h
    base = 2.0 + (up - 0.5) * 1.4
    n = fbm(p.X * 0.38 + 3, p.Y * 0.38, p.Z * 0.38, 11 + k)
    t = base + (n - 0.5) * 2.0
    c = pick(PINK, t)
    blot = fbm(p.X * 0.33, p.Y * 0.33 + 5, p.Z * 0.33, 29 + k)
    if blot > 0.7:
        c = PINK[max(0, int(t) - 1)] if t > 1 else PINK[1]
    return c


def pig_head(p):
    c = pig_skin(p, 1)
    if p.face == "front":
        x, y = p.x, p.y
        if y == 3 and x in (1, 6):
            return (236, 230, 244)  # белок
        if y == 3 and x in (2, 5):
            return (50, 30, 58)
        if y == 2 and x in (1, 2, 5, 6):
            return PINK[1]  # бровь
        if y >= 6:
            return PINK[3] if x in (2, 3, 4, 5) else PINK[2]  # светлая «щека» над пятачком
    if p.face == "top":
        # уши: два треугольника у задних углов
        for (ex, ey) in ((0, 4), (1, 4), (0, 5), (6, 4), (7, 4), (7, 5)):
            if (p.x, p.y) == (ex, ey):
                return PINK[1]
        for (ex, ey) in ((1, 5), (6, 5), (0, 6), (7, 6)):
            if (p.x, p.y) == (ex, ey):
                return PINK[0]
        if p.y == 0:
            return PINK[3]
    if p.face in ("west", "east"):
        a = p.front_dist()
        if p.y == 3 and a == 1:
            return (50, 30, 58)  # глаз сбоку
        if p.y in (0, 1) and a >= 5:
            return PINK[1]  # ухо у темени сбоку
    return c


def pig_snout(p):
    base = (232, 148, 150)
    c = pick([(150, 78, 86), (206, 118, 124), base, (246, 170, 168), (255, 196, 190)], 2 + p.lit() * 0.6 + (hsh(p.x, p.y, 3, 4) - 0.5) * 1.2)
    if p.face == "front":
        if p.y == 1 and p.x in (1, 2):
            return (104, 46, 58)  # ноздри
        if p.y in (0, 2) or p.x in (0, 3):
            return (214, 128, 134) if (p.x + p.y) % 2 else (222, 138, 142)
    return c


def pig_body(p):
    c = pig_skin(p, 2, "z")
    # пятнистые «складки» по бокам и круп чуть темнее
    return c


def pig_leg(p):
    c = pig_skin(p, 3)
    if p.face == "bottom":
        return (102, 54, 62)
    if p.Y > p.h - 2:
        return (118, 64, 72) if p.Y > p.h - 1 else (152, 86, 92)  # копыто
    if p.Y > p.h - 3 and p.Y <= p.h - 2:
        return PINK[1]
    return c


def cow_coat(p, k=0, belly_axis="y"):
    """Шерсть коровы: белые пятна по бурому, порог по трёхмерному шуму."""
    n = fbm(p.X * 0.22 + 1.7, p.Y * 0.22 + 4.1, p.Z * 0.22 + 9.3, 40 + k) * 0.7 + fbm(p.X * 0.55, p.Y * 0.55, p.Z * 0.55, 41 + k) * 0.3
    up = (p.Z / p.d) if belly_axis == "z" else 1 - p.Y / p.h
    white = n > 0.56 - (0.0 if belly_axis != "z" else (0.5 - up) * 0.12)
    ln = fbm(p.X * 0.7, p.Y * 0.7, p.Z * 0.7, 55 + k)
    if white:
        return pick(COW_WHITE, 2.0 + (ln - 0.5) * 3 + (up - 0.5) * 0.8)
    return pick(COW_BROWN, 2.0 + (ln - 0.5) * 3.2 + (up - 0.5) * 0.9)


def cow_head(p):
    c = cow_coat(p, 1)
    if p.face == "front":
        x, y = p.x, p.y
        if y >= 5:  # морда
            m = [(190, 150, 140), (228, 196, 186), (240, 214, 204), (248, 228, 220), (255, 240, 234)]
            c = pick(m, 2.0 + (hsh(x, y, 5, 1) - 0.5) * 1.2 + (0.5 if y == 5 else -0.2 if y == 7 else 0))
            if y == 6 and x in (2, 5):
                return (110, 62, 62)  # ноздри
            return c
        if x in (3, 4) and y < 5:
            return pick(COW_WHITE, 2.4 + (hsh(x, y, 5, 2) - 0.5) * 0.9)  # белая проточина
        if y == 3 and x in (1, 6):
            return (240, 236, 232) if x == 1 else (30, 22, 22)
        if y == 3 and x in (2, 5):
            return (30, 22, 22) if x == 2 else (240, 236, 232)
    if p.face in ("west", "east"):
        a = p.front_dist()
        if p.y == 3 and a == 1:
            return (30, 22, 22)
        if p.y in (2, 3) and a >= 4:
            return COW_BROWN[0]  # ухо
    if p.face == "top":
        if p.y >= 4 and p.x in (0, 1, 6, 7):
            return COW_BROWN[1]
    return c


def cow_body(p):
    return cow_coat(p, 2, "z")


def cow_leg(p):
    c = cow_coat(p, 3)
    if p.face == "bottom":
        return (44, 34, 32)
    if p.Y > p.h - 2:
        return (44, 34, 32) if p.Y > p.h - 1 else (70, 54, 48)  # копыто
    return c


def cow_horn(p):
    h = [(150, 134, 100), (200, 186, 150), (230, 220, 190), (244, 236, 212), (255, 250, 232)]
    return pick(h, 2.2 + (1.2 if p.Y < 1 else 0) + (hsh(p.x, p.y, 2, 6) - 0.5))


def cow_udder(p):
    m = [(170, 100, 106), (222, 140, 146), (238, 164, 166), (248, 188, 188), (255, 212, 208)]
    c = pick(m, 2.0 + (hsh(p.x, p.y, 7, 8) - 0.5) * 1.1 + p.lit() * 0.5)
    if p.face == "front" and p.y >= 4 and p.x in (0, 1, 2, 3) and (p.x, p.y) in ((0, 4), (3, 4), (0, 5), (3, 5)):
        return m[0]  # вымя: четыре соска
    return c


def sheep_skin(p, k=0):
    up = 1 - p.Y / p.h
    n = fbm(p.X * 0.6, p.Y * 0.6, p.Z * 0.6, 60 + k)
    return pick(CREAM, 2.0 + (up - 0.5) * 0.8 + (n - 0.5) * 2.4)


def sheep_head(p):
    """Голова овцы: тёмное серо-бурое лицо, светлая морда, жёлтоватые глаза, уши торчат по бокам."""
    c = pick(SHEEP_FACE, 2.0 + p.lit() * 0.4 + (fbm(p.X * 0.6, p.Y * 0.6, p.Z * 0.6, 62) - 0.5) * 2.0)
    if p.face == "front":
        x, y = p.x, p.y
        if y == 2 and x in (1, 4):
            return (236, 228, 196)  # глаз: светлый верх
        if y == 3 and x in (1, 4):
            return (28, 22, 22)  # зрачок
        if y == 1:
            return SHEEP_FACE[3] if x in (2, 3) else SHEEP_FACE[2]  # лоб
        if y >= 4 and 1 <= x <= 4:  # светлая морда
            c = pick(SHEEP_MUZZLE, 2.2 + (hsh(x, y, 9, 3) - 0.5) * 1.0)
            if y == 5 and x in (2, 3):
                return (74, 52, 52)  # ноздри
            return c
    if p.face in ("west", "east"):
        a = p.front_dist()
        if p.y == 3 and a == 1:
            return (28, 22, 22)
        if p.y == 2 and a == 1:
            return (236, 228, 196)
        if p.y in (1, 2) and 3 <= a <= 5:
            return SHEEP_FACE[0] if a == 5 or p.y == 1 else (150, 108, 102)  # ухо
        if p.y >= 4 and a <= 1:
            return pick(SHEEP_MUZZLE, 2.0 + (hsh(a, p.y, 9, 5) - 0.5))
    if p.face == "top":
        return SHEEP_FACE[3] if p.y <= 1 else pick(SHEEP_FACE, 2.0 + (hsh(p.x, p.y, 9, 6) - 0.5) * 1.4)
    return c


def sheep_body(p):
    # остриженная овца: розовато-бежевая кожа с лёгкими складками
    up = (p.Z / p.d)
    n = fbm(p.X * 0.5, p.Y * 0.5, p.Z * 0.5, 70)
    c = pick(CREAM, 2.2 + (up - 0.5) * 0.9 + (n - 0.5) * 2.2)
    fold = vnoise(p.X * 0.35, p.Y * 1.1, p.Z * 0.35, 71)
    if fold > 0.82:
        c = CREAM[1]
    return c


def sheep_leg(p):
    n = fbm(p.X * 0.6, p.Y * 0.6, p.Z * 0.6, 80)
    if p.face == "bottom":
        return (44, 34, 30)
    if p.Y > p.h - 2:
        return (44, 34, 30) if p.Y > p.h - 1 else (70, 56, 50)
    return pick(SHEEP_LEG, 2.0 + (n - 0.5) * 2.4)


def wool(p, k=0):
    """Шерсть: серый кучерявый узор (множится на цвет шерсти). Кочки — ячейки с дрожащим центром, купол ярче к центру."""
    s = 3.2
    best, second = 9.0, 9.0
    bx = by = bz = 0
    gx, gy, gz = p.X / s, p.Y / s, p.Z / s
    cx, cy, cz = math.floor(gx), math.floor(gy), math.floor(gz)
    for dx in (-1, 0, 1):
        for dy in (-1, 0, 1):
            for dz in (-1, 0, 1):
                ix, iy, iz = cx + dx, cy + dy, cz + dz
                jx, jy, jz = hsh(ix, iy, iz, 90 + k), hsh(ix, iy, iz, 91 + k), hsh(ix, iy, iz, 92 + k)
                ddx, ddy, ddz = ix + jx - gx, iy + jy - gy, iz + jz - gz
                dist = math.sqrt(ddx * ddx + ddy * ddy + ddz * ddz)
                if dist < best:
                    second = best
                    best = dist
                    bx, by, bz = ix, iy, iz
                elif dist < second:
                    second = dist
    dome = 1.0 - min(1.0, best / 0.85)
    ridge = (second - best)
    tone = 2.2 + dome * 1.5 + (hsh(bx, by, bz, 95 + k) - 0.5) * 1.4
    if ridge < 0.12:
        tone -= 1.4  # щель между завитками
    return pick(WOOL, tone)


def chick_feather(p, k=0, axis="y"):
    """Оперение: пёрышки рядами, тёмная кромка снизу каждого, лёгкие полосы."""
    along = p.Y if axis == "y" else p.Y
    n = fbm(p.X * 0.5, p.Y * 0.3, p.Z * 0.5, 100 + k)
    row = (p.Y if axis == "y" else p.Y)
    t = 2.4 + (n - 0.5) * 2.0
    c = pick(FEATH, t)
    return c


def chick_head(p):
    # красный гребешок — шапочка: верх и верхний ряд боков
    if p.face == "top":
        return pick(RED, 2.2 + (hsh(p.x, p.y, 4, 12) - 0.5) * 1.2)
    if p.Y < 1:
        return RED[1] if p.face in ("west", "east", "back") else RED[2]
    if p.Y < 2 and p.face == "front":
        return RED[0]
    c = pick(FEATH, 2.6 + (hsh(p.x, p.y, 4, 13) - 0.5) * 1.4 + p.lit() * 0.4)
    if p.face == "front":
        if p.y == 2 and p.x in (0, 3):
            return (24, 20, 28)  # глаза по краям, заходят на бока
        if p.y == 5 and False:
            return RED[2]
    if p.face in ("west", "east"):
        a = p.front_dist()
        if p.y == 2 and a == 2:
            return (24, 20, 28)
    return c


def chick_beak(p):
    base = 2.4 + p.lit() * 0.5 + (hsh(p.x, p.y, 5, 14) - 0.5) * 0.9
    c = pick(YELLOW, base)
    if p.face == "front":
        if p.y == 1:
            return YELLOW[1]
        if p.x in (1, 2) and p.y == 0:
            return YELLOW[0]  # ноздри
    return c


def chick_wattle(p):
    return pick(RED, 2.2 + (hsh(p.x, p.y, 6, 15) - 0.5) * 1.2 + p.lit() * 0.4)


def chick_body(p):
    # длинная ось тела — Y (0 грудь … 8 хвост), спина +Z. Перья рядами поперёк тела, к хвосту тянутся тёмные
    along = p.Y
    up = p.Z / p.d
    n = fbm(p.X * 0.55, p.Y * 0.35, p.Z * 0.55, 110)
    base = 2.6 + (up - 0.5) * 0.7 + (n - 0.5) * 1.6
    # кромка пёрышка: волна поперёк тела, период 2 пикселя, со сдвигом по рядам
    lat = (p.X if p.face in ("front", "back", "top", "bottom") else p.Z)
    r = math.floor(along / 2.0)
    shift = 1.0 if r % 2 else 0.0
    ph = (along % 2.0)
    tip = 1.6 + 0.35 * abs(((lat + shift) % 2.0) - 1.0) * 2
    if ph > tip:
        base -= 1.0
    if p.face == "bottom" or (p.face == "front" and False):
        base = 2.2 + (n - 0.5) * 1.0
    if p.face == "front":  # брюхо — светлее, пух
        base = 3.0 + (n - 0.5) * 1.2
    return pick(FEATH, base)


def chick_leg(p):
    if p.face == "bottom":
        return YELLOW[0]
    c = pick(YELLOW, 2.2 + p.lit() * 0.3 + (hsh(p.x, p.y, 7, 16) - 0.5) * 0.8)
    if int(p.Y) % 2 == 1 and p.Y > 1.5:
        c = YELLOW[1]  # чешуйки кольцами
    if p.Y < 1:
        return pick(FEATH, 2.2)  # у тела — пух
    return c


def chick_wing(p):
    # крыло 1 × 4 × 6: рабочие грани — боковые (z вдоль крыла, y вниз). Маховые перья рядами, кончики темнее
    n = fbm(p.X * 0.5, p.Y * 0.5, p.Z * 0.5, 120)
    t = 2.4 + (n - 0.5) * 1.4
    if p.Y > p.h - 1.5:
        t -= 1.2
    if int(p.Z) % 2 == 1 and p.Y > 1.0:
        t -= 0.5
    if p.Y < 1:
        t += 0.5
    return pick(FEATH, t)


def zombie_skin(p, k=0):
    n = fbm(p.X * 0.4, p.Y * 0.4, p.Z * 0.4, 130 + k)
    t = 2.0 + (n - 0.5) * 2.1
    c = pick(ZSKIN, t)
    rot = fbm(p.X * 0.3 + 5, p.Y * 0.3, p.Z * 0.3, 140 + k)
    if rot > 0.76:
        c = (96, 120, 70) if t > 1.5 else (74, 96, 56)  # гниль
    return c


def zombie_head(p):
    c = zombie_skin(p, 1)
    if p.face == "front":
        x, y = p.x, p.y
        if y == 2 and x in (1, 2, 5, 6):
            return ZSKIN[0]  # надбровье
        if y == 3 and x in (1, 2, 5, 6):
            return DARK if x in (1, 6) else (46, 56, 48)  # глаза
        if y == 4 and x in (3, 4):
            return ZSKIN[1]  # нос
        if y == 5 and x in (3, 4):
            return ZSKIN[0]
        if y == 6 and 2 <= x <= 5:
            return DARK if x in (2, 5) else (150, 160, 140)  # рот, зубы посередине
        if y == 7 and 2 <= x <= 5:
            return ZSKIN[1]
    if p.face in ("west", "east"):
        a = p.front_dist()
        if 3 <= p.y <= 5 and 3 <= a <= 4:
            return ZSKIN[1] if (a == 3) else ZSKIN[0]  # ухо
    if p.face == "top":
        # клочки волос
        hv = hsh(p.x, p.y, 1, 150)
        if hv > 0.72:
            return (36, 40, 30)
        if hv > 0.5:
            return ZSKIN[1]
    if p.Y < 1 and p.face != "top" and hsh(p.x, 0, p.y, 151) > 0.6:
        return (36, 40, 30)
    return c


def zombie_body(p):
    """Туловище 8×12×4: бирюзовая рваная рубаха, снизу видна кожа."""
    tear = 9.0 + (hsh(int(p.X), 0, int(p.Z), 160) * 3.0 if p.face != "top" else 0)
    skin_below = p.Y > tear and p.face not in ("top",)
    if p.face == "bottom":
        return PANTS[1]
    if skin_below:
        return zombie_skin(p, 2)
    n = fbm(p.X * 0.45, p.Y * 0.5, p.Z * 0.45, 161)
    t = 2.0 + (n - 0.5) * 2.0 + p.lit() * 0.3
    c = pick(SHIRT, t)
    # шов и воротник
    if p.Y < 1.0 and p.face != "bottom":
        c = SHIRT[1] if p.face != "top" else ZSKIN[2]
    if p.face == "top":
        return ZSKIN[1] if (p.X > 2 and p.X < 6) else SHIRT[1]
    # рваные дырки показывают кожу
    hole = fbm(p.X * 0.5 + 8, p.Y * 0.5, p.Z * 0.5, 162)
    if hole > 0.78 and p.Y > 3:
        return zombie_skin(p, 3)
    return c


def zombie_arm(p):
    tear = 3.0 + (hsh(int(p.X * 2), 0, int(p.Z * 2), 170) * 2.0)
    if p.face == "top":  # плечо
        return SHIRT[1]
    if p.face == "bottom":  # кисть
        return ZSKIN[1]
    if p.Y < tear:
        n = fbm(p.X * 0.5, p.Y * 0.5, p.Z * 0.5, 171)
        c = pick(SHIRT, 2.0 + (n - 0.5) * 2.0)
        if p.Y > tear - 1.0:
            c = SHIRT[1]  # оборванный край рукава
        return c
    c = zombie_skin(p, 4)
    if p.Y > p.h - 2:
        return pick(ZSKIN, 1.6)  # кисть темнее
    return c


def zombie_leg(p):
    if p.face == "bottom":
        return SHOE[0]
    if p.Y > p.h - 2:
        return pick(SHOE, 2.0 + (hsh(p.x, p.y, 3, 180) - 0.5) * 1.0) if p.Y < p.h - 1 else SHOE[0]
    n = fbm(p.X * 0.5, p.Y * 0.5, p.Z * 0.5, 181)
    tear = 8.5 + hsh(int(p.X), 0, int(p.Z), 182) * 1.5
    if p.Y > tear and p.Y < p.h - 2 and hsh(int(p.X), int(p.Y), int(p.Z), 183) > 0.35:
        return zombie_skin(p, 5)  # дыра на штанине у лодыжки
    c = pick(PANTS, 2.0 + (n - 0.5) * 2.0 + p.lit() * 0.3)
    if p.Y < 1:
        return PANTS[1]
    return c


# --------------------------------------------------------------------------------------------------------- коробки
# имя → (ш, в, г, рисунок). Размеры — как у кубоидов мобов Minecraft 1.21.1 (модельные пиксели).
BOXES = {
    "pig_head": (8, 8, 8, pig_head), "pig_snout": (4, 3, 1, pig_snout), "pig_body": (10, 16, 8, pig_body), "pig_leg": (4, 6, 4, pig_leg),
    "cow_head": (8, 8, 6, cow_head), "cow_horn": (1, 3, 1, cow_horn), "cow_body": (12, 18, 10, cow_body), "cow_udder": (4, 6, 1, cow_udder),
    "cow_leg": (4, 12, 4, cow_leg),
    "sheep_head": (6, 6, 8, sheep_head), "sheep_body": (8, 16, 6, sheep_body), "sheep_leg": (4, 12, 4, sheep_leg),
    "wool_head": (6, 6, 6, lambda p: wool(p, 1)), "wool_body": (8, 16, 6, lambda p: wool(p, 2)), "wool_leg": (4, 6, 4, lambda p: wool(p, 3)),
    "chick_head": (4, 6, 3, chick_head), "chick_beak": (4, 2, 2, chick_beak), "chick_wattle": (2, 2, 2, chick_wattle),
    "chick_body": (6, 8, 6, chick_body), "chick_leg": (3, 5, 3, chick_leg), "chick_wing": (1, 4, 6, chick_wing),
    "zomb_head": (8, 8, 8, zombie_head), "zomb_body": (8, 12, 4, zombie_body), "zomb_arm": (4, 12, 4, zombie_arm), "zomb_leg": (4, 12, 4, zombie_leg),
}


def layout():
    """Полочная укладка: высокие коробки первыми, слева направо; имя → (u, v)."""
    order = sorted(BOXES, key=lambda n: (-(BOXES[n][1] + BOXES[n][2]), n))
    pos, x, y, shelf = {}, 0, 0, 0
    for name in order:
        w, h, d, _ = BOXES[name]
        bw, bh = 2 * d + 2 * w, d + h
        if x + bw > SIZE:
            x, y, shelf = 0, y + shelf, 0
        pos[name] = (x, y)
        x += bw
        shelf = max(shelf, bh)
    if y + shelf > SIZE:
        raise SystemExit(f"лист {SIZE}×{SIZE} мал: нужно {y + shelf}")
    return pos


def render():
    pos = layout()
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    px = img.load()
    for name, (u, v) in pos.items():
        w, h, d, paint = BOXES[name]
        for face, fu, fv, fw, fh in faces_of(u, v, w, h, d):
            for y in range(fh):
                for x in range(fw):
                    c = paint(point(face, x, y, w, h, d))
                    px[fu + x, fv + y] = (*c[:3], 255)
    return img, pos


def write_uv(pos):
    lines = [
        "// Раскладка листа web/mobs.png: имя коробки → [u, v, ширина, высота, глубина] в пикселях листа.",
        "// Генерируется tools/mob_art.py — руками не править (python -I tools/mob_art.py).",
        f"export const MOB_SHEET = {SIZE};",
        "export const MOB_BOX = {",
    ]
    for name in sorted(pos):
        w, h, d, _ = BOXES[name]
        lines.append(f"  {name}: [{pos[name][0]}, {pos[name][1]}, {w}, {h}, {d}],")
    lines.append("};")
    (ROOT / "web" / "mob_uv.js").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def main():
    img, pos = render()
    img.save(ROOT / "web" / "mobs.png", optimize=True)
    write_uv(pos)
    if len(sys.argv) > 1:
        bg = Image.new("RGBA", img.size, (70, 70, 74, 255))
        bg.alpha_composite(img)
        bg.resize((SIZE * 4, SIZE * 4), Image.NEAREST).save(sys.argv[1])
    print(f"web/mobs.png {SIZE}x{SIZE}, boxes {len(pos)}")


if __name__ == "__main__":
    main()
