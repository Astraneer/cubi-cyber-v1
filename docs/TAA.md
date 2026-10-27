# Опциональное временное сглаживание

TAA разрабатывается отдельно. Умолчание — MSAA4; режим 2 пока не принят как
продуктовая опция. Проверки резолвера не заменяют полную проверку источников
движения и общего View.

`render::taa` обрабатывает single-sample HDR перед Post. Отдельный проход
движения пишет 24 Б/сэмпл: RGBA16Float motion и RGBA32Uint identity. Это не требует
непереносимого 40 Б/сэмпл MRT из HDR, AO, motion и identity. У MSAA4/None нет
дополнительных temporal targets, буферов, запросов или pipeline.

Motion — разность прошлой и текущей UNJITTERED проекции одной геометрической
точки, предсказанная прошлая reverse-depth и valid. Обе clip-проекции используют
одинаковые аппаратные barycentric weights; вычитание builtin pixel centre
вместо текущей геометрической проекции добавляло ошибку субпиксельной сетки.
Текущий jitter вычитается ровно один раз. Identity содержит family/part,
проверенный owner token и generation; Terrain различает каждую ячейку и грань
внутри greedy-квада. Идентичность, глубина, domain и ограничение движения
отклоняют несовместимую историю. Текущая 3×3 HDR-окрестность ограничивает старый
цвет. HDR17 сохраняется; far-plane depth0 допустима. Unknown/неподдерживаемый
identity0 получает только текущий цвет.

Один token связывает scene, poses и resolver. История меняется после GPUdone
и настоящего presentation. Отброшенный кандидат не меняет front. Правки,
отзыв Source, смена Space, camera cut и resize сбрасывают историю. Восемь
центрированных jitter-фаз ограничивают уточнение статики; cached/UI-показ
фазы не продвигает. Восстановление HDR после отмены имеет отдельный token:
кодирование копии не считается queue.submit. Abandon сохраняет dirty-флаг;
старое подтверждение не может принять новую попытку.

Ключ Source учитывает также фактическую ревизию загрузки общего atlas, а не
только внешнюю ревизию material pack. Два разных набора с одинаковой внешней
ревизией поэтому не смешивают прежний цвет; камера, LIGHT и обычный redraw
этот upload epoch не меняют. Это закрывает повтор ревизии после частично
успешного импорта, когда изображения применились, а замена аудио завершилась
ошибкой.

Изображения занимают 88 Б/render pixel: общие motion/identity24 Б, две истории
по 28 Б и стабильный HDR-output8 Б. Для 1280×720 это 81 100 800 Б до source-буферов
и query constants. Лимит TaaConfig —256 МиБ; превышение отвергается до создания
ресурсов. Resolver владеет 64 Б/пиксель; общие 24 Б нельзя учитывать второй раз.
Подготовлен один pipeline; новая сцена пишет 64 Б параметров, делает один
ограниченный проход и копирует 8 Б/пиксель в candidate history. Whole query
включает копию. `pending_work` и `validate_commit` не пишут GPU/метаданные.

Проверено по компонентам:

- CPU/Naga6: оба варианта shader, token/ABA, reset/revoke/Space, bounded phases,
  память и jitter. Native2 GPU проверяют 32×16 authored vectors: HDR17,
  перенос на пиксель, camera-only negative, depth disocclusion, смену
  cell/part/generation, far-plane0, unsupported и атомарный бюджет 63/64 Б.
- Dawn14 на точном production shader: по 512 пикселей, максимальная ошибка 0,
  загрузок не больше 28/пиксель. No-statistics shader тоже компилируется.
  Файлы: `examples/taa_inputs.rs`, `tools/taa_gpu_smoke.mjs`, результат
  `.local/taa-dawn-proof.json`. Это проверка резолвера с authored vectors.
- Реальные Entity/Block используют shared GPU pose и прошлые PRESENTED записи.
  Native и Dawn подтвердили артикуляцию, разные текущий/прошлый jitter,
  exact-zero статику и преимущество настоящего warp над camera-only negative.
  Независимый reference калибрует субпиксельную сетку драйвера;4bits не зашиты
  в production shader. В model shader новые storage bindings в fragment не добавлены.
- Terrain:676 начальных/650 перемещённых/676 rebased пикселей. Независимый f64
  reference проверяет каждый cell/face/owner/generation и прошлую показанную
  матрицу через отменённый кадр. Float16 conversion учитывается явной half-ULP
  границей. First-frame identity записывается при valid-motion0. В 8-slot fixture
  запрошено 2400 Б; на сцену 96 Б uniform и GPU snapshot1024 Б чтения/1024 Б записи.
  1000 idle проверок дают 0 allocations; loading pipeline2.
- Height/Proxy native:252 Height и 546–572 Proxy f64-свидетелей, текущие
  cell/face/triangle, generation ABA, partial-alpha invalidation, discard,
  rebase и 1000 idle0alloc. Четыре grouped draw, шесть loading pipelines;
  fixture8slots использует 1696 Б буферов. Пространства high8: Terrain1,
  Height16, ProxyOpaque17, ProxyCutout18 — без пересечения с model families.
  Actual-View Native/Dawn для этих источников описан ниже.
- Water/Cloud invalidation: native2 и Dawn14 прошли 28 672 независимых пикселя.
  Маски применяются после геометрии/оптики до resolve; подробности и ограничения
  в `TEMPORAL-OPTICS.md`. Отдельные extra targets маскам не нужны.

До включения режима остаётся реальная проверка completion/abandon в обеих
продуктовых оконных оболочках; источники всего View проверены ниже. Малые fixtures не
доказывают время полного мира или качество на оборудовании заказчика.

Environment подключает постоянный TAA HDR output к Post во время loading.
Два native graph теста прошли: первый кадр с нулевым jitter побитно совпал с
обычным выводом при 2048 render pixels и 3200 физических tone pixels; отказ
общего upload/pixel budget не вызывает motion callback или terrain writes.
Отменённый кандидат восстанавливает прежний HDR и cached tone под Manual
экспозицией. AutoExposure с максимальным Bloom даёт 10 Post проходов,
5 timing sources и 27 query pairs — меньше прежних пределов 16/64. Этот
fixture очищает неподдерживаемые motion samples и не заменяет реальные
geometry-source gates. Этот тест повторно прошёл после подключения Post rollback.

Water/Cloud invalidity подключён после настоящих color и geometry-motion
проходов перед resolve, с общим preflight. Обычный путь не создаёт маски.
Совместный View проверен ниже; реальные оптические покрытия дополнительно
проверены отдельными Native/Dawn mask fixtures.

Post rollback отдельно прошёл два native GPU теста: AutoExposure с максимальным
Bloom после отмены побитно совпал с нетронутым обычным Post owner; дальнейшая
адаптация использовала прежнюю гистограмму. Для Manual сохранена текущая
пользовательская компенсация экспозиции. Дополнительный банк занимает 16 Б,
новых pipelines нет; обычный owner не имеет этого банка. Только явно submitted
кандидат требует GPU restore. Unsubmitted discard восстанавливает CPU metadata,
не читая незаписанный банк. Restore budget denial не рисует кадр с новой
экспозицией; abandoned restore остаётся dirty, старый restore token отклоняется.
Гистограмма и Bloom пересчитываются из восстановленного HDR без адаптации,
после чего обычные UI presents снова выполняют только tone.

Общий native View с AO2, Cloud2, GI2, SSR2, AutoExposure и шестью уровнями Bloom
прошёл отдельную проверку capture: реальные model/terrain/far-motion snapshots,
оптическая invalidation и Shadow дают 15 sources и 45 query pairs при прежних
пределах 16/64. Получен завершённый whole-GPU interval; capture не потерял ни
одного источника. Размер изображения96×64 и малые spatial domains выбраны для
функциональной проверки лимита, не для оценки FPS. Файл:
`crates/client/tests/view_temporal_capture.rs`, лог `.local/taa-whole-capture-gpu.log`.

Loading и оконные оболочки имеют guarded hooks: перед заменой View/socket общий
чистый preflight считает88 Б на scaled render pixel ровно один раз, добавляет
фактические front/staging capacities, параметры, запросы и Post16 Б. Превышение
256 МиБ сохраняет прежние renderer/session и input-admission gate и сообщает
требуемые МиБ. Native/WASM Clippy и три CPU preflight теста прошли. После всех
семейств и окончательного resize loading создаёт временные владельцы; каждый
encode получает общий монотонный frame ID. Queue.submit подтверждает отправку,
GPU completion после surface.present принимает общую транзакцию; ошибка encode
вызывает discard. Resize ждёт pending candidate, Browser edit-completion fence
использует его реально закодированную ревизию. Эти hooks пока не являются
проверкой реального GUI в режиме2: вариант2 всё ещё скрыт, разрешены только1/4.

Отдельный `tools/temporal-view-probe` теперь исполняет production Rust View
нативно и через WASM/Dawn с настоящим canvas. Получены восемь фаз статики,
120 пикселей настоящего движения Human под неподвижной камерой и отдельная
деформация костей без object/camera translation (native48/Dawn49 pixels).
Независимые interior-контроли vacated/removed силуэта дали ошибку0: native27/27
и Dawn25/27 свидетелей. Строгие границы остаются ≤2/255 и residual≤0,05.
Обычная MSAA4-картинка той же сцены служит quality-контролем: суммарная L1
ошибка raw1→TAA уменьшилась native29079→13830 и Dawn32283→13348.
Geometry/source epochs при движении не менялись; история действительно
участвовала. Manual и полный AutoExposure/Bloom после abandoned/submitted
кандидатов восстановились побитно. Полный граф сохранил15sources/45querypairs
с завершённым whole-GPU interval. В Dawn это16 сохранённых canvas presents
с GPUdone, surface.present и animation-frame boundary перед commit.

Результаты: `.local/artifacts/temporal-view-probe/{native,dawn}-analysis.json`;
WASM576inputs был byte/mtime-fenced, SHA256
`4532f098f7fd0b2bcaee1fbe28f8bf47ce989012a3bbede97356534208086221`.
CPU bootstrap выполняет production job codecs синхронно и не доказывает
Worker/performance. В этом принятом probe Far owner подготовлен, но фронт пуст.
Следующий отдельный probe требует реальный committed Height+Proxy. Режим2
по-прежнему скрыт; продуктовая оболочка и полный профиль ещё не приняты.

Отдельный actual-View Far probe теперь публикует настоящий фронт25 Height tiles
и8 Proxy pages из явных публичных Height/Geometry пакетов. Он не вызывает
клиентский генератор. Native и WASM/Dawn накопили8 фаз; при сдвиге камеры
получены1181/1361 и1180/1361 валидных движущихся Height/ProxyOpaque pixels.
Geometry/source epochs между статикой и pan сохранились. После подтверждённой
замены source2→3 и удаления дальнего столба92 native/104 Dawn внутренних
свидетеля дали maxError0/residual0. Внутренний контроль независимо требует
плоскую5×5 окрестность текущего raw-reference, чтобы не принимать вновь
открытые пространственные floor/far-clip границы за старый цвет. Минимум16
свидетелей и пределы2/255/0,05 сохранены; ghost-negative отвергается.

Dawn здесь использует настоящие Rust View encode/submit/surface.present,
GPUdone и animation-frame boundary перед общим commit. Все6 сохранённых
кадров имеют complete whole-GPU interval. WASM585inputs fenced, SHA256
`2bdba6b5ba08b7e8584cb986def33a36251da2237adedc73c3ff1fc58975afb6`.
Raw/analysis и сохранённый bundle: `.local/artifacts/temporal-far-probe/`.
Actual-View partial-alpha Cutout roof также прошёл:78 native/84 Dawn
независимых зелёных interior pixels совпали с текущим raw-reference (ошибка0),
все четыре слова прошлой surface identity равны0. После удаления крыши/столба
сохранились92/104 ghost-свидетеля с ошибкой0. Финальный Cutout bundle585inputs:
`acb8335f9fb37cb404d05a8becf0072c8897401a911b5b5a07ff3ec8673fb36b`.
Сохранены opaque и Cutout bundles, raw outputs, hashed native executable и
`proof.json`. Последний Native/WASM Clippy прошёл3,41/26,72с. Существующий
Chest в wholeView даёт микроскопическую статическую погрешность5,7e−6…1,14e−5
физического пикселя; exact-zero всего model View не заявляется. Height/Proxy
статическое движение побитно нулевое.

RecipeBoard19 прошёл компонентные native/Dawn проверки и тот же production
Rust View на обеих платформах:3 карточки на подтверждённой Raw/LIGHT стене,
8 фаз,414 static/436 moving pixels и полный64-bit public session. После
удаления0 IDs; после возврата452 IDs с valid-motion0 нового lifetime.
192 независимых ghost interiors дали maxError0/residual0. Unsubmitted и
submitted-discard сохранили прежний cache побитно и не приняли новый frame.
Native4,71с; все11 сохранённых Dawn records имеют настоящий canvas/GPUdone.
Только тестовая камера приближена к карточкам для достаточного числа interior
свидетелей; границы16/2/0,05 не менялись. Результаты и Native executable:
`.local/artifacts/temporal-boards-probe/`. Сам motion owner добавляет960 Б для
9 карточек, использует прежний MotionTargets pass и не добавляет queries.
Near/full-optics+3 active boards отдельно прошёл Dawn с14sources/44queries;
это раздельный сценарий от committed-Far, прежний15/45 envelope сохранён.

Native и Browser также проверяют resize до изменения Surface/canvas/View.
Отказ временного графа по256 МиБ сохраняет renderer/session; Native просит
вернуть прежний физический размер окна, Browser сохраняет intrinsic canvas.
Повторный тот же размер не форматирует ошибку на каждом RAF; изменение
масштаба или ресурсов разрешает новую проверку. Пользователь получает нужные
МиБ и действующий предел. Это не увеличивает бюджеты создаваемых ресурсов.

Вариант2 доступен только в отдельном неподтверждённом candidate overlay для
продуктовой проверки; опубликованный домен остаётся[1,4]. Native/Browser GUI
с reprepare/reconnect/unsupported resize и F8 ещё должны пройти.
