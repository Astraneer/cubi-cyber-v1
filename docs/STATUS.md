# Статус — 2026-10-26

**M0–M13 продолжаются автономно. M1–M3 приняты Linux+Windows; M4 render-gates прошли Windows/WARP. M5–M13 целиком ещё не приняты.** Среда работает, конфигурация сохранена черновиком. Донор — только §2:780 файлов/12 220 972Б, хеши проверены.

| Этап | Проверенный результат / остаток |
|---|---|
| M0 | Native/WebGPU, CI, MSAA4, лицензии, счётчики. |
| M1 | Версии/Unknown,4 PROBE_REF, native/WASM детерминизм;100000 правок→1 производная задача. |
| M2 | Лес9915квадов×16Б; FLIP=0, Hi-Z66→54объекта без изменения кадра. |
| M3 | Правка видна за1завершённый кадр; физика/полёт/откат проверены. |
| M4 | Windows H5=4,0786×≥2×; побитная глубина/receivers, SMAA, AO/emission, complete View PASS. Mixed-парный замер0,9740× сохранён. |
| M5 | JOIN18Dawn/обаMesa PASS;2seed≤31,98px. Geometry retry/отзыв:7тестов,3старых отрицательных контроля. Сертификаты отдельно от свечения машин:27серверных тестов PASS, старый код падает. Native960×540/H16:512Geometry,22Exact ждут,50,43px>32, Farfront0. Browser1024:3319Height/496Proxy к88,34с; первый HDR занят на120с. |
| M6 | UDP75±20мс/2%:0коррекций; Raw/LIGHT privacy, Forget/ABA/restart. Взрыв10000блоков: оба клиента точны,28770/29275Б. Raw очередь исправлена, реальные socket-тесты PASS. |
| M7 | GPU-инстансинг, ИИ/PvP/смерть/дропы/respawn/WAL;7 owner-сценариев. |
| M8 | Golden выживания/фермы/окон; печь:тик200; заряд180000J. Настоящий Native UI: HP13→17,1яблоко; ведро269→268→269, два Store-restart/та же identity PASS. Browser проверяется следующим. |
| M9 | §8.6:32UDP/5000мобов/10000машин/100000событий — локальный PASS217с; настоящий CI server_scale PASS. Спавн:3линии/42клетки/17компонентов/4активные страницы производят без наблюдателей к тику936; WAL точен. Пользовательская линия×2 ещё впереди. |
| M10 | Native+Dawn GI/вода/SSR/SSAO/labPBR, MSAA/SMAA, Scale85–100%. Optional TAA: общий View/Human/Far/Cutout/доски19 Native+Dawn PASS, включая удаление/смену session; оболочка ещё проверяется, UI2 скрыт. |
| M11 | Owner-крафт/сундук/creative/config/reconnect и F8/80шагов PASS. Физические доски:12реальных рецептов/9панелей, Native+Dawn, opaqueness/depth/Raw+LIGHT revoke PASS. Защищённые окна readonly даже Owner/при старте; совместный тест Store-reopen PASS. Настоящие PNG спавна впереди. |
| M12 | Локальный импорт пака/.minecraft:0HTTP/утечек; WebAudio/CPAL. Owner+публичные Mine/Place cues после durable commit, exactUUID/action, privacy/ретраи/decodedPCM PASS. Два настоящих клиента ещё проверяются; аппаратная слышимость не подтверждена. |
| M13 | F3/F8/HTML/compare,1800оконных кадров. Debug web/native ZIP запущены. Настоящий Windows release ZIP распакован в CI e516: Sky3,WINDOW18/18GPU+HTML,WS ping PASS. Linux/Web release новой версии впереди. |

**Общий снимок:**1663входа, defaultMSAA4; optional TAA исключён. Workspace/all-targets+WASM Clippy и fmt PASS; совместные UI5/Client10/Server4 теста PASS. Дополнительные scoped backend63/audio18/board Native+Dawn/проверки старого→нового proxy сохранены. Удалённый legacy TAA slot сохраняет старые CUI1-байты, скрыт и не выдаётся за применённую настройку.

**Сейчас:** исправляются старт и проверка UI. Общий снимок с раздельными HID-зависимостями прошёл workspace/all-targets Clippy и27серверных тестов; публикация воды сохраняет строгую проверку текущего envelope. Исследуется план с512Geometry/22Exact без повышения лимита. Browser дошёл до Game, следующий общий GPU-барьер истёк; найдено возможное лишнее ожидание чужого HDR, готовится точный UI receipt. Food/Bucket, спавн/readonly/Day–Night и звук двух Native ещё не приняты. PNG проверяются отдельно от счётчиков; дедлайны120/15 сохранены.

[Последний CI5f82c27](https://github.com/Astraneer/cubi-cyber-v1/actions/runs/40406795980): Windows целиком PASS (H5=4,0519×, новые доски/Far и извлечённый ZIP), server_scale PASS; Linux дошёл до настоящего product UI и остановился на Loading timeout120с. DOM race исправлен; deadlines сохранены. [Последний принятый Linux+Windows CI](https://github.com/Astraneer/cubi-cyber-v1/actions/runs/40240830659). PR отсутствуют: GitHub HTTP403; main не менялся. Подробности: [история](STATUS_HISTORY.md), M4–M13, RECIPE_BOARDS.md, M11-CROSSROADS-SERVER.md, BLOCK-AUDIO.md. VPS/DNS/секреты не менялись.
