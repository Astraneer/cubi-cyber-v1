# Статус — 2026-10-26

**M0–M13 продолжаются автономно. M1–M3 приняты Linux+Windows CI; M4–M13 целиком ещё не приняты.** Среда работает, конфигурация сохранена черновиком. Донор — только §2:780 файлов/12 220 972Б, хеши проверены.

| Этап | Проверенный результат / остаток |
|---|---|
| M0 | Native/WebGPU, CI, MSAA4, лицензии, счётчики. |
| M1 | Версии/Unknown,4 PROBE_REF, native/WASM детерминизм;100000 правок→1 производная задача. |
| M2 | Лес9915квадов×16Б; FLIP=0, Hi-Z66→54объекта без изменения кадра. |
| M3 | Правка видна за1завершённый кадр; физика/полёт/откат проверены. |
| M4 | Sun: побитная глубина sparse/full. Windows/WARP1,49× вместо≥2×; Cutout≈531мс. Новый sampler-free PASS/MIXED вариант проверяется. |
| M5 | H4:50построек/8,29млн проверок; два seed≤31,98px при цели32. JOIN18Dawn/обаMesa прошли. Полный1024/Near32: native Game+FarFront получены, подтверждение LOD Error ещё отсутствует. |
| M6 | UDP75±20мс/2%:0коррекций; Raw/LIGHT privacy, Forget/ABA/restart. Взрыв10000блоков: оба клиента точны,28770/29275Б. Очередь Raw исправлена, реальные socket-тесты прошли. |
| M7 | GPU-инстансинг, ИИ/PvP/смерть/дропы/respawn/WAL;7 owner-сценариев. |
| M8 | Golden выживания/жидкостей/фермы/окон; печь:тик200; заряд180000J. Еда32тика и вёдра подключены к авторитетному вводу/WAL; серверные и сетевые тесты прошли. Проверка через UI впереди. |
| M9 | §8.6:32UDP/5000мобов/10000машин/100000событий — локальный PASS217с; последний настоящий server_scale CI зелёный. World+LIGHT после WAL совпадают точно. |
| M10 | Native+Dawn GI/вода/SSR/SSAO/labPBR, SMAA, Scale85–100%. Windows AO/свечение лавы и полный View теперь PASS. Optional TAA: общий View проверяется, UI скрыт. |
| M11 | Свежая пара/native1МиБ: Owner-крафт/сундук/creative/config/reconnect, F8/80шагов прошли. HDR488→HUD489, повтор735→736. Полный профиль ещё не принят. |
| M12 | Реальный локальный импорт пака/.minecraft:0HTTP/утечек; WebAudio/CPAL. Browser: HDR1243→cached HUD1244→ввод. Причинные звуки Owner сохраняются вместе с действием, дубликаты отсеиваются. |
| M13 | F3/F8/HTML/compare,1800оконных кадров. Debug-ZIP web/native распакованы и запущены: Sky3,WINDOW18/18GPU+HTML,WS ping. Release Windows ещё впереди. |

**Последние проверки:** `50e5f65` —1619входов: workspace/all-targets+WASM Clippy PASS. Новый frozen snapshot1625входов: еда/вёдра/звук, обработка отказа начала еды, ожидание LIGHT-upload и диагностика опубликованного LOD; повторные workspace/all-targets+WASM Clippy и fmt PASS. Optional TAA в этот набор не входит.

**Сейчас исправляется:**

1. Тени: последний Windows H5=1,4916×; точность глубины, SMAA, AO/emission и complete View прошли. PASS-материалам подготовлен отдельный проход без fragment shader; mixed сохраняет исходный alpha-test. Windows-результат нового варианта ещё не получен.
2. Полный профиль: native release достиг Game95,11с/FarFront106,40с; маска настроек3/7, отсутствует LOD Error. Browser за120с остался Loading: producer сошёлся, GPU-staging не завершился. Новый gate ждёт реальные LIGHT-upload до тяжёлого первого HDR; условие физического HDR+HUD completion сохранено. Следующая свежая пара проверит исправление и показатели опубликованного LOD.
3. Геймплей: точное подтверждение еды/вёдер, отмена/отказ/повтор/WAL и причинные Owner-звуки покрыты тестами. Готовится проверка мышью/клавиатурой в настоящей Owner-сессии. Слышимость на аппаратном аудиовыходе ещё не подтверждена.

[Последний CI50e5f65](https://github.com/Astraneer/cubi-cyber-v1/actions/runs/40379269839): Linux — последний ProductUI Loading timeout; Windows — H5 cost; server_scale PASS. [Последний принятый Linux+Windows CI](https://github.com/Astraneer/cubi-cyber-v1/actions/runs/40240830659). PR отсутствуют: GitHub HTTP403; main не менялся. Подробности: [история](STATUS_HISTORY.md), M4–M13, PUBLIC-PRIVACY.md, SERVER_SCALE.md. VPS/DNS/секреты не менялись.
