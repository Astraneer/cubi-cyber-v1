# Точный альфа-тест дальних теней

Кандидат сокращает чтения mip0 для public Cutout. Он не меняет PXP2,
геометрию, число draw/compute-команд, размер атласа, GPU bindings или 96 MiB
трёх статических и трёх совмещённых карт 2048².

При загрузке `shadow_alpha::classify_mip0` проверяет каждый alpha byte каждого
слоя реально загружаемого RGBA8 атласа. Если все значения >=128, Y bit1
означает «всегда проходит»; если все <128, Y bit2 означает «всегда отбрасывается».
Смешанный слой выполняет прежний `textureSampleLevel(...,0).a >= 0.5`.
Y bit0 (tint) и Z/W (emission/fallback) сохраняются. Перед быстрым путём
проверяются material ID и фактическое число слоёв texture. Невалидный ID/слой
не становится непрозрачным из-за флага. Это бинарная карта без A2C;
near-шейдер с `textureSampleGrad` сохраняет исходный путь и не использует эти
mip0-факты для других mip.

Классификация требует 4 B на слой CPU; GPU использует прежнюю 16 B таблицу
материалов. Добавлены общий CPU epoch и mutex, GPU ресурсов/пайплайнов нет.
Статические 1000 вызовов не аллоцируют, не загружают данные и не рисуют.

`apply_albedo_in_place` действительно меняет общий color atlas. Поэтому после
любого такого импорта/reset конструкторные факты консервативно отключаются
на весь срок этого TerrainGpu. Каждый upload дополнительно пишет 4 B Y на
слой, затем публикует проверенный монотонный epoch. Проверки размеров, цели,
metadata и переполнения epoch выполняются до GPU writes. Повторная публикация
metadata не возвращает отключённые флаги. Новый TerrainGpu может заново
классифицировать новый атлас.

ShadowGpu потребляет именно этот epoch: при изменении инвалидирует все
статические каскады и combined cache, даже если Sun, камера, геометрия и
public source keys не изменились. Готовность ждёт обновления всех активных
каскадов; неактивные каскады сохраняют обязанность обновления до включения.
Параметры разрешения/числа карт и лимиты работы не повышались.

## Проверки и границы

- CPU/Naga: 2 теста, alpha0/127/128/255/mixed, ошибки размеров, tint/Z/W и
  порядок проверок границ точного production shader.
- Native: одинаковая глубина с отключёнными фактами для всех пяти alpha
  случаев. Шесть настоящих inplace/reset обновлений при фиксированном Sun и
  source обновляют staticNear и combinedFar; alpha0/127 не оставляют глубину,
  reset восстанавливает исходное изображение. Отказ Target не меняет epoch/
  изображение; включение ранее неактивных каскадов ждёт их обновления.
  1000 idle: 0 alloc/upload/draw.
- Dawn: 11 случаев, реальный production shader и 64 public Cutout quads.
  Полные depth images побитно равны sampling reference. Неправильные ID/слои
  с принудительным PASS не рисуют; ложный PASS для alpha0 даёт 136 ошибочных
  depth pixels и подтверждает чувствительность контроля. Повторные upload
  0/mixed/reset с выключенными фактами точны. Лог
  `.local/shadow-alpha-dawn.log`, JSON `.local/shadow-alpha-dawn-proof.json`.
- Исходный H5 None TOTAL, currentSun12/2048/96 MiB на Linux llvmpipe25.2.8:
  sparse114.350 ms/full822.813 ms, 7.1955×; глубина BITWISE и независимые
  352 dark/10048 clear receivers сохранены. Optional Some node query overhead
  измерен отдельно и не включён в строгий исходный порог 2×.

Сравнение флагов ON/OFF использует один и тот же fixture: 1 public object,
64 Cutout quads, 6 currentSun кадров, 2048, одинаковый ShadowWork, одинаковая
полная глубина. Все числа ниже — суммы GPU TOTAL, не аппаратный FPS:

| Порядок | Atlas | Факты, ms | Sampling, ms | Sampling / факты |
| --- | --- | ---: | ---: | ---: |
| Всегда ON первым (исходный отрицательный результат) | весь alpha255 | 87.786 | 58.401 | 0.665 |
| Всегда ON первым | mixed | 61.327 | 64.655 | 1.054 |
| Чередование ON/OFF первым | весь alpha255 | 51.364 | 72.472 | 1.411 |
| Чередование ON/OFF первым | mixed | 62.671 | 74.951 | 1.196 |

Разброс и mixed-контроль не позволяют объявить общее ускорение по этим
коротким Linux прогонам. В частности, mixed по-прежнему читает texture.
Предыдущий Windows WARP baseline проваливал H5 (1.380× в node diagnostic,
позднее 1.64×); общий Cutout занимал около554 ms из660 ms sparse. Новый actual
Windows `50e5f65` с данным кандидатом также провалил исходный порог 2×:
None sparse609.9895 ms/full909.8766 ms (1.4916×). Отдельный Some diagnostic
по-прежнему показывает Cutout530.9567/514.6134 ms. Это отрицательный результат
специализации через helper: нельзя считать, что чтение texture фактически
устранено данным D3D12 compiler path. Проверка GPU metadata/quad histogram
готовится отдельно, чтобы отличить неверную публикацию flags от исполнения
дорогого пути. Ускорение продукта с сотнями дальних объектов и смешанной
листвой не доказано.

Кандидат public occupied BBox остаётся отдельным CPU proof и не включён:
actual WARP Opaque был менее1 ms и не объясняет доминирующий Cutout.

## Структурный кандидат после отрицательного WARP результата

После `50e5f65` Alpha-помощник больше не является единственным механизмом
ускорения. GPU cull читает живую shared metadata каждого публичного quad:

- невалидный material/layer или FAIL (в том числе противоречивые flags6)
  не попадает в draw;
- PASS попадает в depth-only pipeline с `fragment: None`: fragment shader,
  sampler и texture fetch в этом draw отсутствуют структурно;
- MIXED выполняет исходный mip0 sampler и альфа-тест.

Две группы используют прежнюю pair-арену: PASS растёт от начала, MIXED — от
конца. При непересекающихся валидированных payload spans P+M<=Q<=C; поэтому
atomic reservations не пересекаются. C определяется размером visible span,
не физическим размером payload binding. VS сохраняет исходную математику
позиции; отдельный partial entry меняет только индекс tail-list.

Учёт изменений явный: compute storage bindings5→7 (лимит8), indirect48→64 B,
startup pipelines5→6, сгруппированные Far draws3→4. Арена, PXP2, texture/map
размеры и лимиты payload не растут. PASS и MIXED входят в ОДИН CutoutDraw
query interval; исходный TOTAL порог H5 2× не изменён.

Proxy owner сохраняет Arc общего atlas epoch. Distant owner имеет точный
`cull_key=(front_pair,atlas_epoch)` и per-cascade epoch готовых списков.
Даже прямой draw со старым PASS list после upload запрещён до нового cull.
При этом ShadowGpu по-прежнему инвалидирует static+combined карты; одних
новых списков для смены alpha недостаточно. Foreground cull не читает World,
не строит vertices и не вызывает generation.

Новые доказательства:

- Native stress128×8 descriptors: полная1024-pair арена, точные множества
  PASS/MIXED и их уникальность, oversized payload binding, invalid IDs/layers,
  flags6, tint, collect lowbit и повторные empty resets — PASS0.40 s.
- Native lifecycle2.04 s: исходные пять alpha cases, real imports/reset,
  Target rejection, inactive cascades, запрет stale PASS, точный cull epoch
  при неизменной геометрии, 1000idle0alloc/upload/draw — PASS.
- Dawn13: production compute и обе graphics pipeline против ИСХОДНОГО
  ordinary-quad VS + sampler reference. Полные depth images BITWISE, включая
  public fixture544 quads с одновременно272 PASS и272 MIXED. PASS128/255
  подают64 quads без fragment shader, FAIL/invalid/flags6 — 0. После import
  с отключёнными фактами все64 идут в MIXED, старый PASS не возвращается.
  Логи `.local/shadow-buckets-{alpha,coexisting}-dawn.log`.
- Исходный Native None TOTAL currentSun12/2048/96 MiB: sparse192.386 ms,
  full1198.413 ms,6.229×; depth BITWISE и352dark/10048clear сохранены.
  Отдельный Some Cutout interval включает ОБЕ группы:11.920/12.432 ms.
- Парный чередующий порядок, один fixture/6currentSun кадров/64 quads,
  work EXACT иdepth BITWISE: uniform fragmentNone49.125 ms / sampling59.391 ms
  (1.209×); mixed90.890/77.109 ms (0.848×, отрицательный результат сохранён).
  Для mixed обе стороны выполняют тот же sampler. Короткие вариативные LVP
  числа не подтверждают производительность всего продукта.

Actual Windows WARP для СТРУКТУРНОГО кандидата ещё требуется; приведённый выше
fail1.4916× относится к предыдущему helper-only `50e5f65`. Приёмка полного
профиля с сотнями public casters и смешанной листвой остаётся отдельной.
