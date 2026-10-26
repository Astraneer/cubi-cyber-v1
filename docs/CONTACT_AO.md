# Контактное затенение окружающего света

При аудите SSAO был только пунктом настроек. Существовал voxel vertex AO;
отдельного screen-space прохода не было. Hi-Z prepass содержит лишь opaque
Terrain buckets0/3, поэтому не годится как полный источник глубины моделей
и cutout-геометрии.

Компонент `render/src/contact_ao.rs` с `contact_ao.wgsl`, optional ambient
MRT материалов и полный Environment-граф проверены на native GPU.
Реальный игровой переключатель и browser-продукт остаются отдельной приёмкой. Режим0 возвращает `None` до
любого вызова устройства и не расходует upload budget. Старый NoAO граф
не должен создавать новый target/layout/pipeline.

Режимы1/2 используют16/32 фиксированных направлений полусферы, радиус1,25
мировой единицы, bias0,025 и strength1. API допускает радиус0,001–64;
нечисловые/неограниченные значения отклоняются до GPU allocation. Kernel хранится точными IEEE754
словами, одинаковыми при native/WASM загрузке; вращения по кадрам и noise
нет. Один sample-frequency fullscreen pass восстанавливает поверхность
из текущей обратной projection и реальной глубины полного opaque/model
кадра. Нормаль получается из ближних известных соседей с выбором меньшего
перепада; при недостатке соседей применяется visibility1. Используется
`@interpolate(linear,sample)` UV, положение аппаратных MSAA samples не
предполагается по умолчанию. Depth0 и выход за экран не создают occluder.

Входы — HDR, signed ambient color delta и depth **одного и того же** готового
кадра. Выход `max(HDR − ambient_delta × (1 − AO), 0)` сохраняет прямой
свет Солнца и свечение. Разность бывает отрицательной у тёмной аннотации
порта поверх `max(light, glow)`:
удаление ambient тогда восстанавливает цвет emission floor, а не гасит его.
Этот случай дополнительно сравнивается с тем же f64 geometry oracle.
Ambient MRT обязан содержать только действительно удаляемую разность цвета:
RGB block light, sky SH и GI. Для `max(light, glow)` это разность полного
цвета и цвета с нулевой ambient, чтобы emission floor сохранился. Небо,
вода и облака не входят в этот opaque ambient MRT; compositing выполняется
до Water/Aerial/Cloud. Alpha каждого MSAA sample сохраняется. Это не TAA,
не temporal history и не физический интеграл всей полусферы.

Пока не меняются source revision и camera, повторный encode даёт0
upload/pass/draw/query. Изменение radiance source с той же камерой не
переписывает camera uniform. Вся проверка sample/upload budget происходит
до GPU-записей. Resize создаёт только targets/bind и16B image metadata,
не компилирует pipeline. Pipeline1 создаётся во время loading; uniform672B.

Память выходных targets:8B/pixel при MSAA1 и40B/pixel при MSAA4 (четыре
HDR samples + один resolve). Отдельной AO texture нет. Будущий ambient MRT
добавит8/32B/pixel соответственно, то есть полный optional AO graph требует
16/72B/pixel сверх NoAO. При1280×720/MSAA4 это63,28125MiB +672B uniform;
это размеры descriptor, не измеренная VRAM драйвера. Work сообщает точные
composition samples `width×height×MSAA` и консервативные **max** depth reads
`composition×(5+16/32)`; actual FS atomic counters доступны только в
необязательном debug-режиме. Обычный материал не получает storage binding.

Проверка `cargo test --locked -p render --test contact_ao -- --nocapture`:

| MSAA/качество | Certified f64 receivers | Максимальная HDR ошибка | Constant-AO negative |
|---|---:|---:|---:|
|1 /16|234|0,00097520|0,111306|
|1 /32|234|0,00096594|0,093000|
|4 /16|202|0,00129539|0,060546|
|4 /32|197|0,00124994|0,061153|

Эталон пересекает лучи с независимо заданными f64 floor/wall planes,
нормали берёт из геометрии; GPU normal/depth shader в эталон не переносится.
Измерены реальные sample positions адаптера. В радиационный эталон
передаются наблюдаемые значения RGB16Float входа, чтобы не подменять
rounding устройства выдуманным nearest rounding. Порог0,002 покрывает
финальные half-output/resolve; геометрические границы с неоднозначностью
менее1e-4 исключены явно, остальные witnesses перечислены в таблице.
Полностью белая AO-карта проваливает независимый отрицательный контроль.

Flat floor, depth Unknown и direct/emissive-only источники совпадают с
немодифицированным actual source resolve побитно, включая смешанные
MSAA edges. Настоящие debug counters проверены против budgets. Отказы по
work/upload не меняют очередь публикации; cached encode не запускает query.
Лог `.local/contact-ao-f64-gpu.log`.

`node tools/contact_ao_gpu_smoke.mjs` запускает фактический Dawn/Tint:
20 сочетаний1/4MSAA ×16/32samples ×flat/corner/Unknown/direct-only/signed-glow,
ошибок shader/validation0. Flat/Unknown/direct-only изображения побитно
равны source; углы дают175/326/727/1405 occluded sample hits, depth reads
не выходят за фиксированный предел. Логи `.local/contact-ao-dawn.log` и
`.local/contact-ao-dawn-proof.json`. Компонентные цифры не являются полной приёмкой продукта или обещанием
времени RTX. Проверки общего графа и View описаны ниже; игровой settings
receipt и browser product с включённым эффектом остаются следующими условиями.


## Полный граф и общий View

`cargo test --locked -p render --test contact_ao_graph -- --nocapture`
проверяет настоящий граф Terrain с включённым local PBR pack, HeightGpu,
LodProxyGpu, Human, Chest, AtmosphereSky и Celestial. Все классы используют
один optional MRT render pass, затем ContactAO и Post. Native GPU прошёл
все сочетания MSAA1/4 × quality1/2:

| MSAA/качество | Samples с контактом | Geometry / sky samples | Проверки direct HDR floor |
|---|---:|---:|---:|
|1 /16|44|1448 /2648|2571 RGB channels|
|1 /32|53|1448 /2648|2571 RGB channels|
|4 /16|190|5685 /10699|10310 RGB channels|
|4 /32|244|5685 /10699|10310 RGB channels|

Проверяется исходный HDR, signed MRT delta, глубина и итог **каждого**
MSAA sample. Итог не опускается ниже `HDR − positiveAmbientDelta` с
half-format допуском0,01 для яркого direct HDR. Все sky samples совпали
побитно. Отдельный взгляд вверх реально выводит небо/Солнце: delta0 и
passthrough exact. `Some(quality0)` побитно совпал с `None`, дополнительный
MRT/его байты отсутствуют. Повтор неизменённого кадра даёт0 работы;
resize64²→80×48 сохраняет compiled pipelines. Лог
`.local/contact-ao-joint-gpu.log`.

В `client/tests/view_gi.rs` actual View отдельно прошёл NoAO и AO1: реальные
GI worker jobs9728,4 probes и69 синих model pixels при CPU RGB/Sky0;
ShadowSome и View-prepared model MRT совместимы. AO1 кодирует1 проход,
1000 idle iterations дают0 аллокаций/работы. Эти данные не заменяют
interactive native/browser screenshots и настройки. Старые шесть
Environment-проверок, включая MSAA1/4 и Water/Aerial/Cloud/Post, также
прошли с NoAO. Exact production Model MRT проверен в Dawn, legacy first
HDR output сохранился побитно; Sky/Height/Proxy новые варианты покрыты
полным native графом.
