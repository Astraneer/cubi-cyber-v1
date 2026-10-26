# Локальные материалы и LabPBR

`render::material_pack` принимает заимствованные локальные PNG. Файловых, сетевых,
серверных и экспортных API в нём нет. `client::local_materials` сопоставляет обычные
имена `minecraft/textures/block/*` существующим слоям реестра; слой и геометрия не
перенумеровываются. Отсутствующие изображения сохраняют исходный атлас.

До декодирования проверяются размеры/число слоёв, суммарный encoded/decoded budget
64 МиБ и GPU budget 64 МиБ; запись PNG ограничена 16 МиБ, сторона — 4096, слои — 256.
Результирующая сторона по умолчанию не выше 128. Обычные вертикальные animation strips
дают первый полный кадр; APNG отклоняется. PNG CRC/усечение не публикуют частичный набор.
Полная цепочка mip: albedo в sRGB с фильтрацией в линейном свете и alpha weighting,
normal/specular — линейные, tint — отдельный R8. Категориальные metal ID не усредняются
в выдуманное диэлектрическое F0. Анимация следующих кадров пока не выполняется.

Публичная документация LabPBR 1.3 подтверждает:

| Карта | R | G | B | A |
|---|---|---|---|---|
| `_n` | normal X | normal Y, DirectX/Y-down | AO | высота |
| `_s` | perceptual smoothness | dielectric F0 / metal ID | porosity 0–64, SSS 65–255 | emission 0–254; 255 — нет |

Все n/s каналы линейные. Шероховатость `(1−R)²`; normal Z восстанавливается.
RGB `_s` получает alpha=255, то есть не превращается в светящийся материал.
POM ограничен 16 выборками и масштабом 0,06 блока, затухает от 4 до 8 блоков;
по умолчанию выключен, host включает его только для C/D near-профилей.
Porosity/SSS декодируются и сохраняются, полноценное объёмное SSS пока не реализовано.

Официальный shaderlabs сайт отвечал CONNECT 403. Использованы только публичные
описания формата, не реализации чужих рендереров:

- [Channel reference](https://github.com/jasonjgardner/comfy-ui-minecraft-pbr#channel-reference),
  SHA256 снимка `69e03dd5cf970009d785776c6326575073f307d298ba50d08ffb8a7f8e507372`.
- [LabPBR format](https://github.com/jasonjgardner/minecraft-pbr-conversion#labpbr-format),
  SHA256 `edca77a2f8647f1de8908c1399dd61b46f7c69b8ef5bcf4f2e3f2e84628df26d`.
- [Независимая таблица каналов LabPBR 1.3](https://github.com/TheFantasticLoki/MC-PBR-Utility#pbr-channel-layouts),
  SHA256 `619f7ebea0a79bd75e814d6d97e0edf7a34523dd02566fbe5ea06e61e0c4ce57`.

Первый источник перечисляет восемь металлов в порядке iron/gold/aluminum/chrome/
copper/lead/platinum/silver и диапазон iron230…silver237. Это основание сопоставления
230–237; независимая полная нормативная таблица промежуточных ID пока не получена.
ID сохраняется без потери. 255 означает custom albedo-as-F0. Reserved238–254
не активируются рендерером; ранее действующий набор остаётся целым.

F0 для пресетов — явно физическая аппроксимация, **не каноническая RGB-таблица LabPBR**:

- Fe/Au/Al/Cu/Pt/Ag: опубликованные sRGB значения таблицы `fNormalMetals` в
  [Filament, Physical material](https://google.github.io/filament/Filament.html),
  преобразованные стандартной sRGB transfer function в linear. SHA256 документа
  `64e2843fd054e921821fb58a06ed974f3a036e06d7b62ae71a5260cf4e2e42ff`.
- Cr: [Rakić et al., 1998](https://doi.org/10.1364/AO.37.005271), Brendel–Bormann n/k,
  [публичные CC0 научные данные](https://github.com/polyanskiy/refractiveindex.info-database/blob/master/database/data/main/Cr/nk/Rakic-BB.yml),
  SHA256 `92f53df1385bd35ad83a3d10f222c6fd2360bc5f3dc12e09087e87fb29091559`.
- Pb: [Werner et al., 2009](https://doi.org/10.1063/1.3243762),
  [CC0 данные](https://github.com/polyanskiy/refractiveindex.info-database/blob/master/database/data/main/Pb/nk/Werner.yml),
  SHA256 `4bd6688c7fef081e97f6b50d93690eeb3cb9cb8c59300e6fed933aaa8360aa66`.

Для Cr/Pb n,k линейно интерполированы на 610/550/460 нм, затем
`F0=((n−1)²+k²)/((n+1)²+k²)`: Cr=(.643027346,.651117231,.662150269),
Pb=(.906284547,.923337095,.929386526). Это три длины волны, не интеграл CIE;
результат зависит от источника и поверхности. Научные файлы находятся только в
локальном cache; в код внесены фактические производные числа и ссылки.

Отдельный loading-only shader variant сохраняет исходный NoPack pipeline/layout.
Он применяет normal, AO, dielectric/conductor F0, GGX roughness и emission. AO действует
на ambient/reflected свет, не повторно на direct sun; emission не входит в GI source.
Подготовленный вариант позволяет заменить GPU bank без компиляции в игровом кадре.
Light/GI ресурсы сохраняются при любом порядке загрузочных attachment.

Встроенная лава (`L_LAVA=185`) излучает `linear_albedo × 1` независимо от AO,
известного нулевого SKY/RGB и Солнца. Terrain и непрозрачный Proxy читают флаг и
коэффициент из уже существующей таблицы материалов 16 Б/слой; новых GPU bindings,
текстур и геометрии нет. NoPack и PBR без `_s` дают одинаковую эмиссию; исходные
байты default `_s` сохранены. Явный локальный `_s` заменяет fallback: 0/255 выключают
эмиссию, 1–254 дают `alpha/254`. Near сохраняет попиксельный канал; Proxy использует
среднее по слою — пространственные emissive-маски вдали пока не воспроизводятся.
Emissive-слагаемое не входит в удаляемую ambient-delta контактного AO.

`builtin_emission` прошёл на настоящем software Vulkan: AA1/4, 14 кадров,
2688 HDR-проверок Terrain/Proxy; ContactAO побитно сохраняет известную тёмную сцену
с эмиссией. Контроли с тем же albedo и без эмиссии остаются чёрными при ненулевом
покрытии. Смена `_s`/отключение пака: 0 пересборок geometry/pipeline, таблица
обновляется ровно на 2976 Б только при изменении effective metadata; повторная
эквивалентная metadata не загружается. CPU проверяет оба sentinel и коэффициенты;
Naga→HLSL, Clippy и WASM проходят. Это компонентная проверка, не GPU benchmark.
Windows/WARP в текущем CI обнаружил нулевой выход ContactAO при ненулевом
входном HDR; причина ещё не установлена. Строгое сравнение сохранено, сообщение
содержит AA/frame/первое отличие. Два независимых AA1/AA4-контроля проверяют
`SV_Position`, UV, фиксированный mip0, sample index и AO strength0 в том же
encoder; восемь случаев побитно проходят Vulkan. Это диагностика границы
платформы, а не подтверждённое исправление или ослабление эмиссионной проверки.

Четыре теста `cargo test -p render --test material_pack` прошли на настоящем software Vulkan:
повреждения/budgets/atomic publication; GPU каналы/DirectX/sentinels/POM; реальное
изображение terrain с изменяемыми albedo/normal/emission, NoPack zero pixel diff и
нулём pipeline compilations при замене банка. Это не аппаратный GPU benchmark.
Отдельный тест подтвердил одинаковое изображение при обоих порядках подключения
материалов/light/GI и сохранение старого банка при reserved ID. Сгенерированный
lit+GI+PBR WGSL прошёл независимую Naga→HLSL трансляцию; это не заменяет FXC/DX12 CI.
Основной браузерный импорт прошёл настоящий OS picker: исходные synthetic albedo/n/s
32×32 применены к 11 слоям, загружено 3 617 700 Б GPU, проверено 92 113 пурпурных
пикселей настоящего terrain. Ни одной pipeline compilation внутри кадра. Последующая
`.minecraft` выборка без изображений вернула исходные материалы. При этом 422 активных
WebSocket-кадра не содержали canary/аккаунтных байтов, новых HTTP-запросов — 0.
Артефакты: `.local/artifacts/product-ui-smoke.json`, `product-local-materials.png`.
Синтетические тестовые изображения создаются во временной папке, не хранятся как
игровой ресурс. В общие shadow/entity bindings обновляется существующий 16px proxy;
сам terrain читает полное разрешение импортированного банка. Pixel oracle выполнен
на программном GPU и не доказывает бюджет на RTX. Native Windows picker/import worker подготовлен и MSVC-проверен; фактический OS dialog
в Windows и DX12 CI остаются отдельными проверками.
