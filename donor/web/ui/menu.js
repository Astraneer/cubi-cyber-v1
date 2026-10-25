// Общее меню: пауза (Esc), настройки (O, шестерёнка в окне инвентаря), справка «Управление», экран приветствия.
// Пока меню открыто, мир на паузе (main.js: тики стоят, пока open) и ввод в мир не идёт: мышь и клавиши меню дальше
// корня не всплывают, а клавиши вне меню перехватываются на window в фазе захвата. Закрытие возвращает туда, откуда
// открыли: в игру — через input.play() (полный экран по настройке и захват мыши); меню закрывается, только когда
// мышь захвачена: пока браузер держит паузу после Esc — «Возвращаемся…», отказал — меню остаётся с подсказкой
// (никакого «щёлкни» в пустом мире). В окно инвентаря или на экран приветствия — сразу.
// Под заголовком паузы — строка режима: полный экран с захватом клавиатуры или окно браузера (Ctrl и Alt отключены,
// бег — двойное W или запасная клавиша). Потеря фокуса с открытым окном инвентаря — тоже пауза.
// Отладка из адреса: &menu=pause | settings | controls | <раздел> (graphics, world, controls, interface, sound,
// debug) | settings:<раздел>; &msearch=слово — поиск.
import { icon, pixelText } from "./icons.js";
import { keys, keyLabel } from "./keys.js";
import { createSettingsView, liveInfo, PRESET_ICON } from "./settings_view.js";
import { debugOverlay } from "./debug_overlay.js";

const FOCUSABLE = "button:not([disabled]):not([hidden]), input:not([disabled]), [tabindex='0']";
const WELCOMED = "voxel-welcomed";

export function createMenu({ S, model }) {
  if (!document.querySelector('link[href*="ui/styles.css"]')) { // на сайте адрес с ?v=<сборка> (tools/stamp_web.mjs)
    const l = document.createElement("link");
    l.rel = "stylesheet"; l.href = new URL("./styles.css", import.meta.url).href;
    document.head.appendChild(l);
  }
  const ctx = { input: null, screen: null, prof: null, help: null };
  const root = document.createElement("div");
  root.className = "vm";
  root.tabIndex = -1;
  root.setAttribute("role", "dialog");
  root.setAttribute("aria-modal", "true");
  root.innerHTML = `
    <div class="vm-screen vm-card vm-pause" data-screen="pause" aria-label="Пауза">
      <div class="vm-head"><span data-title="Пауза"></span><p class="vm-sub">Мир остановлен</p></div>
      <div class="vm-note vm-mode" data-mode hidden>${icon("info")}<div></div></div>
      <div class="vm-btns">
        <button class="vm-btn primary" data-do="resume">${icon("play")}<span data-resume>Продолжить</span><kbd>Esc</kbd></button>
        <div class="vm-auto vm-resume-note" data-resume-note hidden></div>
        <button class="vm-btn" data-do="settings">${icon("settings")}<span>Настройки</span><kbd data-key="settings"></kbd></button>
        <button class="vm-btn" data-do="controls">${icon("keyboard")}<span>Управление</span></button>
        <button class="vm-btn" data-do="profiler">${icon("activity")}<span>Профайлер</span><em data-state="profiler"></em><kbd>⇧<i data-key="profiler"></i></kbd></button>
        <button class="vm-btn" data-do="fullscreen">${icon("maximize")}<span>Полный экран</span><em data-state="fullscreen"></em><kbd>F11</kbd></button>
        <button class="vm-btn" data-do="bug" title="Кадр, отчёт и запись ввода — разработчику">${icon("alert")}<span>Сообщить о баге</span><kbd data-key="bugreport"></kbd></button>
      </div>
      <div class="vm-quick">
        <div class="vm-qtop"><span>Качество</span><span class="vm-qname" data-preset-name></span>
          <span class="vm-qauto" title="Авто: держит цель кадров/с и поднимает качество до потолка, пока есть запас">Авто<button class="vm-switch" role="switch" data-qauto aria-label="Авто-качество"></button></span></div>
        <div class="vm-seg" role="radiogroup" aria-label="Потолок качества">${model.presets.map((p) =>
          `<button role="radio" data-qpreset="${p.id}" title="Потолок «${p.label}»: ${p.text}">${icon(PRESET_ICON[p.id] || p.icon)}<span>${p.label}</span></button>`).join("")}</div>
        <div class="vm-qlive" data-qlive><span class="n" data-qfps>—</span><span data-qline></span></div></div>
    </div>
    <div class="vm-screen vm-card vm-help" data-screen="controls" aria-label="Управление">
      <header class="vm-top">
        <button class="vm-icbtn" data-do="back" title="Назад (Esc)" aria-label="Назад">${icon("back")}</button>
        <div class="vm-ttl"><h2>Управление</h2><p>Все клавиши игры. Переназначить — в настройках, раздел «Управление».</p></div>
        <button class="vm-icbtn" data-do="close" title="Закрыть" aria-label="Закрыть">${icon("x")}</button>
      </header>
      <div class="vm-scroll"><div class="vm-cheat" data-cheat></div>
        <div class="vm-note">${icon("info")}<div data-modes></div></div>
      </div>
      <div class="vm-help-foot">
        <button class="vm-textbtn" data-do="fullscreen">${icon("maximize")}<span>Полный экран</span></button>
        <button class="vm-textbtn acc" data-do="rebind">${icon("keyboard")}<span>Изменить клавиши</span></button>
      </div>
    </div>`;
  document.body.appendChild(root);

  let open = false, view = null, from = null;
  // &uidemo=auto — пример состояния авто для снимков вида без видеокарты (?lite в SwiftShader авто не успевает
  // замерить): 52 к/с при цели 60, авто сняло MSAA и снижает SSAO. Только вид меню; игра и авто не меняются.
  let demo = null;
  const makeDemo = () => {
    const RS = Object.create(S);
    if (S.msaa) RS.msaa = false;
    return { RS, fps: { n: 60, fps: 52, ms: 19.2, low1: 41, limit: "" },
      status: { on: true, enabled: true, target: 60, mode: "fill", why: "52 к/с: видеокарта 19.4 мс на кадр — снижаю SSAO",
        parts: S.msaa ? ["без MSAA"] : [], line: "" } };
  };
  const stack = [];

  // ---------------------------------------------------------------- мир под меню и приветствием: заморозка
  // Пока меню открыто (и «Экономия в меню» включена, профайлер закрыт), мир не рисуется вовсе. main.js в конце кадра
  // зовёт freeze(холст, мир догружен, разбудить): снимается этот кадр уменьшенной копией (≈ 480 точек по длинной
  // стороне; размытие и затемнение — один раз, вместе с холстом HUD: хотбар, окно инвентаря), показывается под
  // интерфейсом, и «разбудить» держится здесь — больше кадров main.js не просит. Правка настройки — кадры предпросмотра
  // (не меньше 250 мс, пока мир догружается — до 3 с), потом новый снимок; закрытие меню — снова игра.
  // Приветствие (#help, без меню) — та же заморозка: мир снимается, когда догружен (или через HELLO_WAIT — иначе
  // плитки не догрузились бы вовсе), и размывается один раз. backdrop-filter по живому миру на большом экране стоит
  // половину кадра (docs/rules/pitfalls.md), поэтому ни меню, ни приветствие его не используют.
  const HELLO_WAIT = 30000;
  // Пока мир под приветствием грузится, кадры идут, а снимок обновляется раз в HELLO_EVERY мс: под приветствием
  // всё время размытый мир, без смены «резкий живой мир → размытый снимок» в конце загрузки (reports/agent-loadux.md).
  const HELLO_EVERY = 250;
  const snap = document.createElement("canvas"); // снимок под меню (в корне .vm)
  snap.className = "vm-snap";
  snap.setAttribute("aria-hidden", "true");
  root.prepend(snap);
  const hsnap = document.createElement("canvas"); // снимок под приветствием (#help, см. mountWelcome)
  hsnap.className = "vm-snap";
  hsnap.setAttribute("aria-hidden", "true");
  let wake = null, previewAt = 0, helloAt = performance.now(), helloSnapped = false, helloShotAt = -1e9;
  const freezeMode = () => open && !!S.uiSaver && !ctx.prof?.visible;
  const helloMode = () => !open && helpVisible() && !ctx.prof?.visible;
  const shadeMenu = (c, w, h) => { c.fillStyle = "rgba(8, 7, 6, 0.45)"; c.fillRect(0, 0, w, h); };
  // Приветствие — как его фон в ui/styles.css (#help): центр светлее, края темнее.
  const shadeHello = (c, w, h) => {
    const g = c.createRadialGradient(w / 2, h * 0.42, 0, w / 2, h * 0.42, Math.hypot(w, h) / 2);
    g.addColorStop(0, "rgba(8, 7, 6, 0.12)"); g.addColorStop(1, "rgba(8, 7, 6, 0.6)");
    c.fillStyle = g; c.fillRect(0, 0, w, h);
  };
  /** Снимок кадра gl в холст dst: размытие blur (в снимке), затем затемнение shade; HUD — поверх мира. */
  function takeSnap(gl, dst, blur, shade) {
    const W = gl.width, H = gl.height;
    if (!W || !H) return false;
    const k = Math.max(1, Math.max(W, H) / 480);
    const w = Math.max(1, Math.round(W / k)), h = Math.max(1, Math.round(H / k));
    if (dst.width !== w || dst.height !== h) { dst.width = w; dst.height = h; }
    const c = dst.getContext("2d");
    // Размытие с запасом за краем (иначе края темнеют от прозрачного), затем затемнение.
    const m = blur * 3;
    c.filter = blur ? `blur(${blur}px)` : "none";
    c.drawImage(gl, -m, -m, w + 2 * m, h + 2 * m);
    const ui = document.getElementById("ui");
    if (ui && ui.width) c.drawImage(ui, -m, -m, w + 2 * m, h + 2 * m);
    c.filter = "none";
    shade(c, w, h);
    return true;
  }
  function wakeWorld() { const w = wake; wake = null; w?.(); }
  // Правка настройки (окно, профайлер) под замороженным меню или приветствием — кадры предпросмотра, потом новый снимок.
  function preview() { if (!open && !helloMode()) return; previewAt = performance.now(); wakeWorld(); }

  const api = {
    set: (k, v) => { model.set(k, v); applyUi(k); preview(); },
    applyPreset: (id) => { model.applyPreset(id); applyUi("*"); preview(); },
    resetAll: () => { model.resetAll(); applyUi("*"); preview(); },
    /** Мир под меню заморожен (кадры не рисуются): FPS не показывать — «меню: мир на паузе». */
    paused: () => freezeMode(),
    back: () => back(),
    close: () => close(true),
    canBack: () => stack.length > 0,
    toolOn: (id) => (id === "profiler" ? !!ctx.prof?.visible : false),
    toggleTool: (id) => { if (id === "profiler") toggleProfiler(); },
    /** Строка авто-качества (engine/adaptive.js) и честный FPS — или null до connect. */
    autoLine: () => (ctx.auto ? ctx.auto.status().line : null),
    fpsLine: () => (ctx.prof ? ctx.prof.fpsLine() : null),
    /** Состояние авто (status(): что снижено, причина, цель) и сводка FPS профайлера — для живых строк. */
    autoStatus: () => (demo ? demo.status : ctx.auto ? ctx.auto.status() : null),
    fpsInfo: () => (demo ? demo.fps : ctx.prof ? ctx.prof.fpsInfo() : null),
    /** Значения кадра с поправками авто (RS: свои поля — то, что авто сейчас держит ниже ваших). */
    autoRS: () => (demo ? demo.RS : ctx.auto ? ctx.auto.RS : null),
  };
  const sv = createSettingsView({ S, model, api });
  root.appendChild(sv.el);
  const screens = () => root.querySelectorAll("[data-screen]");
  const current = () => root.querySelector(`[data-screen="${view}"]`);

  // ---------------------------------------------------------------- вид

  function applyUi(k = "*") {
    document.documentElement.style.setProperty("--ui-k", String(S.uiScale || 1));
    // Размытие живого мира (backdrop-filter) дорого на большом экране; с профайлером мир под меню живой — без размытия,
    // чтобы замер был про игру. Замороженный мир размыт один раз в снимке (freeze).
    root.classList.toggle("no-blur", !S.uiBlur || !!ctx.prof?.visible);
    root.classList.toggle("no-anim", !S.uiAnim);
    root.classList.toggle("no-hints", !S.uiHints);
    document.body.classList.toggle("no-debug-text", !S.debugText);
    if (k === "uiHints" || k === "uiScale") { if (view === "settings") sv.rerender(); renderTitles(); }
    syncPause();
  }
  // Заголовки пиксельным шрифтом: целый масштаб от размера шрифта меню.
  function renderTitles(scope = root) {
    const fs = parseFloat(getComputedStyle(root).fontSize) || 16;
    for (const slot of scope.querySelectorAll("[data-title]")) {
      const px = Math.max(2, Math.round(fs * (parseFloat(slot.dataset.px) || 0.25)));
      if (slot.dataset.drawn === `${px}`) continue;
      slot.replaceChildren(pixelText(slot.dataset.title, px));
      slot.dataset.drawn = `${px}`;
    }
  }
  function syncPause() {
    for (const k of root.querySelectorAll("[data-key]")) k.textContent = keys.label(k.dataset.key);
    const st = (name, on) => { const e = root.querySelector(`[data-state="${name}"]`); if (e) { e.textContent = on ? "вкл" : "выкл"; e.classList.toggle("on", on); } };
    st("profiler", !!ctx.prof?.visible);
    st("fullscreen", !!document.fullscreenElement);
    for (const b of root.querySelectorAll('[data-do="fullscreen"]')) b.disabled = ctx.input ? !ctx.input.canFullscreen : false;
    const md = root.querySelector("[data-mode]");
    if (md) { const t = modeText(); md.hidden = !t; md.lastElementChild.textContent = t; }
    syncResume();
    const cur = model.presetOf(), p = model.presets.find((x) => x.id === cur);
    for (const b of root.querySelectorAll("[data-qpreset]")) b.setAttribute("aria-checked", String(b.dataset.qpreset === cur));
    const n = root.querySelector("[data-preset-name]");
    if (n) n.textContent = p ? p.label : "Свой";
    // «Авто» в паузе — та же настройка, что карточка «Качество» в «Графике»; без «Держать цель» не работает.
    const qa = root.querySelector("[data-qauto]");
    if (qa) {
      qa.setAttribute("aria-checked", String(S.autoQuality !== false));
      qa.closest(".vm-qauto").classList.toggle("dep-off", S.holdFps === false);
    }
    syncAuto();
  }
  // Живая строка под пресетами паузы (и в «Графике» — sv.syncAuto): сейчас N к/с, держится ли цель и почему нет, что
  // снижено. Пока меню открыто — раз в 0.5 с.
  function syncAuto() {
    const q = root.querySelector("[data-qlive]");
    if (q) {
      const st = api.autoStatus(), L = liveInfo(st, api.fpsInfo(), api.paused());
      const f = q.querySelector("[data-qfps]");
      f.textContent = L.fps == null ? "—" : `${L.fps}`; f.style.color = L.color;
      q.querySelector("[data-qline]").textContent = L.short;
      q.classList.toggle("low", !!st?.why && !L.paused);
    }
    sv.syncAuto?.();
  }
  setInterval(() => { if (open) syncAuto(); }, 500);
  // Бег без Ctrl: «двойное W или Caps Lock» — по текущим назначениям.
  function runText() {
    const alt = keys.active("sprintSafe") ? ` или ${keys.label("sprintSafe")}` : "";
    return `бег — двойное ${keys.label("forward")}${alt}`;
  }
  /** Строка режима под заголовком паузы (game/input.js: mode). */
  function modeText() {
    const inp = ctx.input;
    if (!inp) return "";
    const m = inp.mode;
    if (m === "full") return "Полный экран: все клавиши у игры, Ctrl+W вкладку не закроет. Выйти из полного экрана — удерживать Esc.";
    if (m === "fs") return `Полный экран без захвата клавиш (браузер не умеет): Ctrl и Alt в игре отключены, ${runText()}.`;
    if (!inp.canFullscreen) return `Окно браузера: Ctrl и Alt в игре отключены (Ctrl+W закрыл бы вкладку), ${runText()}.`;
    return S.fsPlay ? `Сейчас окно браузера: Ctrl и Alt в игре отключены, ${runText()}. «Продолжить» вернёт полный экран.`
      : `Окно браузера: Ctrl и Alt в игре отключены (Ctrl+W закрыл бы вкладку), ${runText()}. Полный экран — F11.`;
  }
  function renderControls() {
    const k = (id) => `<kbd>${keyLabel(keys.code(id)) === "—" ? "—" : keys.label(id)}</kbd>`;
    const m = (t) => `<kbd>${t}</kbd>`;
    const hot = Array.from({ length: 9 }, (_, i) => keys.code(`hotbar${i + 1}`));
    const hotDefault = hot.every((c, i) => c === `Digit${i + 1}`);
    const it = (name, ...ks) => `<div class="it"><span>${name}</span><span class="k">${ks.join("")}</span></div>`;
    const sec = (title, items) => `<section><h3>${title}</h3>${items.join("")}</section>`;
    // Бег: клавиша на Ctrl / Alt / Win — с пометкой «в полном экране»; запасная и двойное W — везде.
    const runFull = keys.risky(keys.code("sprint")) ? "Бег в полном экране" : "Бег";
    const runAny = keys.code("sprintSafe") ? [k("sprintSafe"), " или 2×", k("forward")] : ["2×", k("forward")];
    root.querySelector("[data-cheat]").innerHTML = [
      sec("Движение", [it("Ходьба", k("forward"), k("left"), k("back"), k("right")), it("Прыжок, всплыть", k("jump")),
        it("Присесть", k("sneak")), it(runFull, k("sprint")), it("Бег везде", ...runAny),
        it("Полёт вкл / выкл", k("flight")), it("Полёт: вверх / вниз", k("jump"), k("sneak"))]),
      sec("Действия", [it("Ломать и бить", m("ЛКМ")), it("Ставить, открыть верстак", m("ПКМ")), it("Выбрать блок", m("СКМ")),
        it("Инвентарь и крафт", k("inventory")), it("Выбросить", k("drop")), it("Выбросить стопку", m("Ctrl"), k("drop"))]),
      sec("Хотбар", [it("Слоты", ...(hotDefault ? [m("1"), " … ", m("9")] : hot.map((c) => m(keyLabel(c))))), it("Листать слоты", m("Колесо"))]),
      sec("Интерфейс", [it("Пауза и меню", m("Esc")), it("Из полного экрана", "держать ", m("Esc")),
        it("Полный экран вкл / выкл", m("F11")), it("Настройки", k("settings")), it("Скрыть интерфейс", k("hud")),
        it("Вид от первого лица / сбоку", k("view")),
        it("Список игроков (держать)", k("playerList")), it("Чат", k("chat"), " или ", k("command")),
        it("Отладка: кратко / подробно", k("profiler")), it("Профайлер", m("Shift"), k("profiler"))]),
      sec("Отладка", [it("Полёт сквозь стены", k("fly")), it("Набор предметов", k("kit")), it("Сдвиг на 10 блоков", k("shift")),
        it("Дальность", k("viewDown"), k("viewUp")), it("Блоков вокруг", k("nearDown"), k("nearUp")), it("Тени", k("shadows")), it("Время", k("dayRun"))]),
    ].join("");
    // Режимы: что делают клавиши в полном экране и в окне браузера; текущий отмечен.
    const cur = ctx.input?.mode;
    const now = (on) => (on ? " <em class=\"vm-now\">сейчас</em>" : "");
    root.querySelector("[data-modes]").innerHTML =
      `<b>Полный экран</b>${now(cur === "full")} — «Играть», «Продолжить» или ${m("F11")}: все клавиши у игры, Ctrl+W, ` +
      `Ctrl+Q, F5 и Alt браузер не трогает; ${m("Esc")} — пауза, удерживать ${m("Esc")} — выйти из полного экрана; ` +
      `Alt+Tab работает.<br><b>Окно браузера</b>${now(cur && cur !== "full")} (и полный экран в браузерах без захвата ` +
      `клавиатуры — Firefox, Safari): Ctrl, Alt и Win в игре отключены — Ctrl+W закрыл бы вкладку; ${runText()}. ` +
      `Закрыть или перезагрузить вкладку во время игры — только с подтверждением.<br>Мышь: ${m("Esc")} отпускает, ` +
      `«Продолжить» возвращает (после Esc браузер ждёт около секунды — кнопка подождёт сама). Потеря фокуса окна ` +
      `(Alt+Tab) — пауза, все клавиши отпускаются.`;
  }

  function showScreen(v, tab, q) {
    view = v;
    for (const s of screens()) s.classList.toggle("on", s.dataset.screen === v);
    renderTitles();
    if (v === "settings") sv.show(tab, q);
    if (v === "pause") syncPause();
    if (v === "controls") { renderControls(); root.querySelector('[data-screen="controls"] [data-do="back"]').hidden = !stack.length; }
    // Фокус: первая кнопка (клавиатура сразу работает; рамка — только после клавиш, :focus-visible).
    requestAnimationFrame(() => {
      if (!open || view !== v) return;
      if (v === "settings") sv.focusFirst();
      else current()?.querySelector(FOCUSABLE)?.focus();
    });
  }
  /** Открыть экран v ("pause" | "settings" | "controls"); origin — откуда: game | inventory | help | paused. */
  function show(v, origin, tab, q) {
    if (!open) {
      open = true;
      from = origin || "game";
      stack.length = 0;
      resumeNote = "";
      root.classList.add("open");
      document.body.classList.add("vm-open");
      // Мир замрёт со следующим кадром (freeze): до снимка — только затемнение, без размытия живого мира.
      root.classList.toggle("freezing", freezeMode());
      debugOverlay()?.setPaused(freezeMode());
      applyUi();
      // Из приветствия мир заморожен: меню снимает свой кадр — кадры снова (wake пуст, если мир шёл кадрами).
      wakeWorld();
    } else if (view && view !== v) {
      stack.push(view);
    }
    showScreen(v, tab, q);
  }
  function back() {
    if (stack.length) showScreen(stack.pop());
    else close(true);
  }
  /**
   * Закрыть всё; resume — вернуться в игру, если открывали из игры: меню ждёт захвата мыши (input.play) и закрывается
   * по нему (main.js: onLock → setVisible(false)); не вышло — остаётся на паузе с подсказкой.
   */
  function close(resume = true) {
    if (!open) return;
    const toGame = resume && (from === "game" || from === "paused") && ctx.input && !ctx.screen?.open && !helpVisible();
    if (toGame && !ctx.input.locked) { tryResume(); return; }
    finish();
  }
  function finish() {
    if (!open) return;
    open = false; view = null; from = null; stack.length = 0;
    resuming = false; resumeNote = "";
    syncResume();
    sv.cancelCapture();
    if (root.contains(document.activeElement)) document.activeElement.blur();
    root.classList.remove("open", "snap", "freezing");
    document.body.classList.remove("vm-open");
    // Мир — снова кадрами: разбудить main.js (время сна в игру и замеры не идёт — там же).
    previewAt = 0;
    debugOverlay()?.setPaused(false);
    wakeWorld();
  }
  // Возврат в игру: жест (кнопка, щелчок) — полный экран по настройке и мышь; Esc жестом не считается — тогда только
  // мышь (под захватом клавиатуры браузер её отдаёт, в окне — нет, и меню просит нажать «Продолжить»).
  let resuming = false, resumeNote = "";
  function tryResume() {
    if (resuming || !ctx.input) return;
    const gesture = navigator.userActivation ? navigator.userActivation.isActive : true;
    resuming = true; resumeNote = "";
    syncResume();
    ctx.input.play({ gesture }).then((ok) => {
      resuming = false;
      if (!open) return;
      if (ok || ctx.input.locked) { finish(); return; }
      resumeNote = gesture ? "Браузер не отдал мышь — нажми «Продолжить» ещё раз."
        : "По Esc браузер мышь не возвращает — нажми «Продолжить».";
      if (view !== "pause") { stack.length = 0; showScreen("pause"); }
      syncResume();
    });
  }
  function syncResume() {
    const b = root.querySelector('[data-do="resume"]');
    if (b) {
      b.classList.toggle("busy", resuming);
      b.setAttribute("aria-busy", String(resuming));
      b.querySelector("[data-resume]").textContent = resuming ? "Возвращаемся…" : "Продолжить";
    }
    const n = root.querySelector("[data-resume-note]");
    if (n) { n.hidden = !resumeNote; n.textContent = resumeNote; }
  }
  const helpVisible = () => !!ctx.help && ctx.help.style.display !== "none";
  function toggleProfiler() {
    if (ctx.prof) ctx.prof.setVisible(!ctx.prof.visible);
    // Профайлер под меню — живые кадры (замерять есть что); закрыли — снова заморозка.
    if (open) { root.classList.toggle("freezing", freezeMode()); debugOverlay()?.setPaused(freezeMode()); preview(); }
    root.classList.toggle("no-blur", !S.uiBlur || !!ctx.prof?.visible);
    syncPause(); sv.sync();
  }
  // Кнопка «Полный экран» показывает, что сейчас, и переключает — вместе с настройкой «Играть в полном экране».
  function toggleFullscreen() {
    ctx.input?.setFullscreen(!document.fullscreenElement);
    syncPause();
  }
  document.addEventListener("fullscreenchange", () => { syncPause(); fillWelcome(); if (view === "controls") renderControls(); });

  // ---------------------------------------------------------------- мышь

  // Клики меню не уходят в мир и в окно инвентаря (их слушатели — на window).
  for (const t of ["mousedown", "mouseup", "mousemove", "click", "dblclick", "wheel", "pointerdown", "pointerup"]) {
    root.addEventListener(t, (e) => e.stopPropagation(), { passive: t === "wheel" });
  }
  root.addEventListener("contextmenu", (e) => { e.stopPropagation(); if (e.target.tagName !== "INPUT") e.preventDefault(); });
  root.addEventListener("click", (e) => {
    const b = e.target.closest("button");
    if (!b || sv.el.contains(b)) return; // кнопки настроек ведёт settings_view.js
    const d = b.dataset.do;
    if (d === "resume") close(true);
    else if (d === "settings") show("settings");
    else if (d === "controls") show("controls");
    else if (d === "profiler") toggleProfiler();
    else if (d === "fullscreen") toggleFullscreen();
    else if (d === "bug") ctx.bug?.();
    else if (d === "rebind") show("settings", null, "controls");
    else if (d === "back") back();
    else if (d === "close") close(true);
    else if (b.dataset.qpreset) { api.applyPreset(b.dataset.qpreset); syncPause(); }
    else if (b.dataset.qauto !== undefined) { api.set("autoQuality", S.autoQuality === false); syncPause(); }
  });

  // ---------------------------------------------------------------- клавиатура

  // Tab не уходит из меню: внутри — обычный порядок браузера, на краях — по кругу.
  const visible = (x) => x.offsetParent !== null && getComputedStyle(x).visibility !== "hidden";
  function trapTab(e) {
    const scr = current();
    const list = [...(scr?.querySelectorAll(FOCUSABLE) || [])].filter(visible);
    if (!list.length) return;
    const a = document.activeElement, first = list[0], last = list[list.length - 1];
    if (!scr.contains(a) || a === scr) { e.preventDefault(); (e.shiftKey ? last : first).focus(); }
    else if (!e.shiftKey && a === last) { e.preventDefault(); first.focus(); }
    else if (e.shiftKey && a === first) { e.preventDefault(); last.focus(); }
  }
  function onKey(e) {
    e.stopPropagation();
    if (view === "settings" && sv.key(e)) return;
    // F8 — баг-репорт и из меню (кадр — мир под меню, engine/bugreport.js будит его сам).
    if (keys.is("bugreport", e.code)) { e.preventDefault(); if (!e.repeat) ctx.bug?.(); return; }
    if (e.code === "Escape") {
      e.preventDefault();
      if (e.repeat) return;
      // Назад по экранам — сразу; закрыть меню — по отпусканию короткого нажатия: удержание Esc в полном экране —
      // выход из него (браузер), а не возврат в игру.
      if (stack.length) back(); else escAt = performance.now();
      return;
    }
    if (e.code === "Tab") { trapTab(e); return; }
    if (e.repeat) return;
    // F3 — отладочный экран краткий ↔ подробный, Shift+F3 — профайлер: как в игре (main.js).
    if (keys.is("profiler", e.code)) { e.preventDefault(); if (e.shiftKey) toggleProfiler(); else debugOverlay()?.toggle(); return; }
    if (keys.is("settings", e.code)) { e.preventDefault(); if (view === "settings") back(); else show("settings"); return; }
    // Стрелки вверх-вниз — по кнопкам паузы и справки.
    if ((e.code === "ArrowDown" || e.code === "ArrowUp") && view !== "settings") {
      const list = [...current().querySelectorAll(".vm-btn, .vm-seg button, .vm-textbtn, .vm-icbtn")].filter((x) => visible(x) && !x.disabled);
      const i = list.indexOf(document.activeElement);
      e.preventDefault();
      list[(i + (e.code === "ArrowDown" ? 1 : -1) + list.length) % list.length]?.focus();
    }
  }
  root.addEventListener("keydown", onKey);
  let escAt = 0; // когда в меню нажали Esc (закрытие — по короткому отпусканию)
  addEventListener("keyup", (e) => {
    if (e.code !== "Escape" || !escAt) return;
    const quick = performance.now() - escAt < 500;
    escAt = 0;
    if (open && quick) back();
  }, true);
  // Клавиши вне меню: при открытом меню — тоже в меню (дальше не идут); при закрытом — O открывает настройки.
  addEventListener("keydown", (e) => {
    if (open) {
      // Поле ввода вне меню (строка описания баг-репорта) — клавиши ему.
      if (!root.contains(e.target) && !/^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) onKey(e);
      return;
    }
    if (!keys.is("settings", e.code) || e.repeat || e.ctrlKey || e.altKey || e.metaKey) return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return;
    e.preventDefault();
    e.stopPropagation();
    const origin = ctx.input?.locked ? "game" : ctx.screen?.open ? "inventory" : helpVisible() ? "help" : "paused";
    show("settings", origin);
    if (origin === "game") ctx.input.unlock(); // отпускает страница — вернуть мышь браузер даст без паузы
  }, true);
  // Потеря фокуса: с открытым окном инвентаря — тоже пауза (мир не тикает, пока игрок в другом окне).
  addEventListener("blur", () => {
    sv.cancelCapture();
    if (!open && ctx.screen?.open) show("pause", "inventory");
  });

  // ---------------------------------------------------------------- экран приветствия (#help)

  function fillWelcome() {
    const h = ctx.help;
    if (!h) return;
    for (const e of h.querySelectorAll("[data-k]")) e.textContent = keys.label(e.dataset.k);
    // Кнопка «Полный экран» на приветствии — настройка «Играть в полном экране» (её применит «Играть»).
    const can = ctx.input ? ctx.input.canFullscreen : true;
    const fs = h.querySelector("[data-fs]"), st = h.querySelector("[data-fs-state]");
    if (fs) { fs.disabled = !can; fs.setAttribute("aria-pressed", String(!!S.fsPlay && can)); }
    if (st) { st.textContent = S.fsPlay && can ? "вкл" : "выкл"; st.classList.toggle("on", !!S.fsPlay && can); }
    const tip = h.querySelector("[data-mode-tip]");
    if (tip) {
      tip.textContent = S.fsPlay && can
        ? "«Играть» откроет полный экран: клавиши не уходят браузеру, Ctrl+W вкладку не закроет. Выйти — удерживать Esc."
        : `Без полного экрана Ctrl и Alt в игре отключены — Ctrl+W закрыл бы вкладку; ${runText()}.`;
    }
  }
  function mountWelcome() {
    const h = ctx.help;
    if (!h || !h.querySelector(".vm-welcome")) return;
    let welcomed = false;
    try { welcomed = !!localStorage.getItem(WELCOMED); } catch {}
    if (!welcomed) h.classList.add("first");
    // Мир под приветствием — снимок (freeze); до него — затемнение. Приветствие ушло («Играть», захват мыши) — мир снова кадрами.
    h.prepend(hsnap);
    if (helpVisible()) h.classList.add("freezing");
    new MutationObserver(() => {
      if (helpVisible()) return;
      h.classList.remove("snap", "freezing"); helloSnapped = false; wakeWorld();
    }).observe(h, { attributes: true, attributeFilter: ["style"] });
    // Логотип (надпись пиксельным шрифтом), «первый запуск» и место под иконки — уже в index.html: приветствие не
    // перестраивается, когда загрузится main.js; здесь иконки только получают свой рисунок.
    for (const ic of h.querySelectorAll("[data-icon]")) ic.outerHTML = icon(ic.dataset.icon);
    fillWelcome();
    const mark = () => { try { localStorage.setItem(WELCOMED, "1"); } catch {} };
    h.addEventListener("click", (e) => {
      const b = e.target.closest("button");
      if (b && b.dataset.menu !== undefined) { e.stopPropagation(); show(b.dataset.menu, "help"); return; }
      if (b && b.dataset.fs !== undefined) {
        e.stopPropagation();
        if (!b.disabled) { api.set("fsPlay", !S.fsPlay); sv.sync(); fillWelcome(); }
        return;
      }
      mark(); // дальше — main.js: щелчок начинает игру
    }, true);
  }
  keys.onChange(() => { fillWelcome(); syncPause(); if (view === "controls") renderControls(); });
  addEventListener("resize", () => { if (open) renderTitles(); preview(); });

  applyUi();

  return {
    el: root,
    get open() { return open; },
    get view() { return view; },
    /** Меню просит редкие кадры: открыто, экономия включена, профайлер закрыт. */
    get idle() { return open && !!S.uiSaver && !ctx.prof?.visible; },
    /**
     * Конец кадра (main.js schedule): true — мир спит, пока меню не позовёт wakeFn (кадр снят и показан под меню);
     * false — рисовать дальше (меню закрыто, живой режим или кадры предпросмотра). idleWorld — плитки догружены.
     */
    freeze(gl, idleWorld, wakeFn) {
      const menu = freezeMode(), hello = !menu && helloMode();
      if (!menu && !hello) { root.classList.remove("snap", "freezing"); return false; }
      if (previewAt) {
        const t = performance.now() - previewAt;
        if (t < 250 || (!idleWorld && t < 3000)) return false;
        previewAt = 0;
      }
      // Приветствие ждёт, пока мир догрузится: кадры же и грузят плитки (engine/frame.js) — но не дольше HELLO_WAIT.
      if (hello && !helloSnapped && !idleWorld && performance.now() - helloAt < HELLO_WAIT) {
        const now = performance.now();
        if (now - helloShotAt >= HELLO_EVERY && takeSnap(gl, hsnap, 3, shadeHello)) {
          helloShotAt = now;
          ctx.help.classList.add("snap"); ctx.help.classList.remove("freezing");
        }
        return false;
      }
      if (menu) {
        if (takeSnap(gl, snap, S.uiBlur ? 3 : 0, shadeMenu)) { root.classList.add("snap"); root.classList.remove("freezing"); }
      } else if (takeSnap(gl, hsnap, 3, shadeHello)) {
        ctx.help.classList.add("snap"); ctx.help.classList.remove("freezing"); helloSnapped = true;
      }
      wake = wakeFn;
      return true;
    },
    /** Ссылки на ввод, окно, профайлер, экран приветствия и авто-качество (после createInput в main.js). */
    connect({ input, screen, prof, help, q, auto, bug }) {
      if (q?.get("uidemo") === "auto") demo = makeDemo();
      Object.assign(ctx, { input, screen, prof, help, auto, bug });
      // F11 и кнопка «Полный экран» меняют настройку «Играть в полном экране» — через меню (сохранить, показать).
      input?.setPrefHandler?.((k, v) => { api.set(k, v); sv.sync(); fillWelcome(); });
      mountWelcome();
      const m = q?.get("menu");
      if (m) {
        const [a, b] = m.split(/[:/]/);
        const tabs = model.tabs.map((t) => t.id);
        if (a === "pause" || a === "controls") show(a, "paused");
        else show("settings", "paused", tabs.includes(a) ? a : b, q.get("msearch") || undefined);
      }
    },
    show,
    close,
    back,
    /** Правка значения снаружи (клавиши main.js): сохранить, применить, обновить меню. */
    set(k, v) { api.set(k, v); sv.sync(); },
    refresh() { applyUi(); sv.sync(); },
    /** Разбудить замороженный мир на кадры предпросмотра (снимок кадра для баг-репорта, engine/bugreport.js). */
    preview,
    /** Совместимость со старой панелью: true — открыть паузу (если закрыто), false — закрыть без захвата мыши. */
    setVisible(v) { if (v) { if (!open) show("pause", "game"); } else close(false); },
  };
}
