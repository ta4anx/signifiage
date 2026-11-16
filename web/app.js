/* Signifiage LAN remote — dependency-free, mobile-first, textContent only. */
(function () {
  "use strict";

  var TOKEN_KEY = "signifiage.token";
  var LANG_KEY = "signifiage.lang";
  var LOCALE_KEY = "signifiage.locale";
  var POLL_MS = 2000;
  var RETRY_MAX_MS = 15000;
  var LOCK_MS = 450;

  var I18N = { en: {}, tr: {} };
  var state = {
    token: null,
    lang: "en",
    langExplicit: false,
    dictReady: false,
    es: null,
    pollTimer: null,
    retryTimer: null,
    retryDelay: 1000,
    lastData: null,
    lastDeckError: null,
    soundSig: null,
    conn: "offline"
  };

  function $(selector, root) {
    return (root || document).querySelector(selector);
  }

  function $$(selector, root) {
    return Array.prototype.slice.call((root || document).querySelectorAll(selector));
  }

  function el(tag, className, text) {
    var node = document.createElement(tag);
    if (className) node.className = className;
    if (text != null) node.textContent = String(text);
    return node;
  }

  function setText(node, value) {
    if (node) node.textContent = value == null ? "" : String(value);
  }

  function storageGet(key) {
    try {
      return window.localStorage.getItem(key);
    } catch (err) {
      return null;
    }
  }

  function storageSet(key, value) {
    try {
      window.localStorage.setItem(key, value);
    } catch (err) {
      /* private mode / file://: the session still works until reload */
    }
  }

  function clamp(value, min, max) {
    return Math.min(max, Math.max(min, value));
  }

  function num(value, fallback) {
    var n = Number(value);
    return Number.isFinite(n) ? n : fallback;
  }

  function formatClock(seconds) {
    if (seconds == null || seconds === "") return "--:--";
    var total = Math.round(Number(seconds));
    if (!Number.isFinite(total)) return "--:--";
    var sign = "";
    if (total < 0) {
      sign = "+";
      total = Math.abs(total);
    }
    var hours = Math.floor(total / 3600);
    var minutes = Math.floor((total % 3600) / 60);
    var secs = total % 60;
    var mm = String(minutes).padStart(2, "0");
    var ss = String(secs).padStart(2, "0");
    return sign + (hours > 0 ? hours + ":" + mm + ":" + ss : mm + ":" + ss);
  }

  function safeColor(value) {
    if (typeof value !== "string") return null;
    return /^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(value) ? value : null;
  }

  /* ---------- i18n ---------- */

  function t(key, vars) {
    var table = Object.prototype.hasOwnProperty.call(I18N, state.lang) ? I18N[state.lang] : null;
    var value = table && Object.prototype.hasOwnProperty.call(table, key) ? table[key] : null;
    if (value == null) {
      value = Object.prototype.hasOwnProperty.call(I18N.en, key) ? I18N.en[key] : null;
    }
    if (value == null) return null;
    if (vars) {
      value = String(value).replace(/\{(\w+)\}/g, function (match, name) {
        return Object.prototype.hasOwnProperty.call(vars, name) ? String(vars[name]) : match;
      });
    }
    return String(value);
  }

  function loadI18n() {
    return fetch("i18n.json", { cache: "no-cache" })
      .then(function (res) {
        if (!res.ok) return null;
        return res.json();
      })
      .then(function (data) {
        if (!data || typeof data !== "object") return;
        I18N.en = data.en && typeof data.en === "object" ? data.en : {};
        I18N.tr = data.tr && typeof data.tr === "object" ? data.tr : {};
        state.dictReady = true;
      })
      .catch(function () {
        /* file:// or offline: the English fallback text in index.html stays. */
      });
  }

  function applyStaticI18n() {
    document.documentElement.lang = state.lang;
    var title = t("app.name");
    document.title = title || "Signifiage";

    $$("[data-i18n]").forEach(function (node) {
      var value = t(node.getAttribute("data-i18n"));
      if (value != null) node.textContent = value;
    });
    $$("[data-i18n-placeholder]").forEach(function (node) {
      var value = t(node.getAttribute("data-i18n-placeholder"));
      if (value != null) node.setAttribute("placeholder", value);
    });
    $$("[data-i18n-aria-label]").forEach(function (node) {
      var value = t(node.getAttribute("data-i18n-aria-label"));
      if (value != null) node.setAttribute("aria-label", value);
    });
    $$(".lang-btn").forEach(function (btn) {
      btn.setAttribute("aria-pressed", btn.getAttribute("data-lang") === state.lang ? "true" : "false");
    });

    setConnState(state.conn);
    if (state.lastData) render(state.lastData);
  }

  function setLang(lang, explicit) {
    state.lang = lang === "tr" ? "tr" : "en";
    if (explicit) {
      state.langExplicit = true;
      storageSet(LANG_KEY, state.lang);
    }
    applyStaticI18n();
  }

  function preferredLang() {
    var stored = storageGet(LANG_KEY);
    if (stored === "tr" || stored === "en") return stored;
    var lastServer = storageGet(LOCALE_KEY);
    if (lastServer === "tr" || lastServer === "en") return lastServer;
    var nav = (navigator.language || "").toLowerCase();
    return nav.indexOf("tr") === 0 ? "tr" : "en";
  }

  /* ---------- connection ---------- */

  function setConnState(kind) {
    state.conn = kind;
    var node = $("#connectionState");
    if (!node) return;
    node.setAttribute("data-state", kind);
    node.textContent = kind === "online"
      ? (t("connect.connected") || "Connected")
      : (t("connect.offline") || "Reconnecting…");
  }

  function showConnect(message) {
    var panel = $("#connectPanel");
    var main = $("#main");
    if (panel) panel.hidden = false;
    if (main) main.hidden = true;
    var error = $("#connectError");
    if (error) {
      error.hidden = !message;
      error.textContent = message || "";
    }
    var input = $("#tokenInput");
    if (input && !input.value && state.token) input.value = state.token;
    setConnState("offline");
  }

  function authFailed() {
    stopStreams();
    showConnect(t("connect.invalidToken") || "The token was rejected.");
  }

  function reportUnreachable() {
    if (state.lastData) return;
    showConnect(t("connect.unreachable") || "Cannot reach the Signifiage server.");
  }

  function start() {
    stopStreams();
    state.lastData = null;
    state.lastDeckError = null;
    var panel = $("#connectPanel");
    var main = $("#main");
    if (panel) panel.hidden = true;
    if (main) main.hidden = false;
    setConnState("offline");
    fetchState();
    connectSSE();
  }

  function fetchState() {
    if (!state.token) return Promise.resolve();
    return fetch("/api/state", {
      headers: { "X-Signifiage-Token": state.token },
      cache: "no-store"
    })
      .then(function (res) {
        if (res.status === 401) {
          authFailed();
          return null;
        }
        if (res.status === 409) {
          renderNoProject();
          setConnState("online");
          return null;
        }
        if (!res.ok) {
          setConnState("offline");
          reportUnreachable();
          return null;
        }
        return res.json();
      })
      .then(function (data) {
        if (data && typeof data === "object") applyState(data);
      })
      .catch(function () {
        setConnState("offline");
        reportUnreachable();
      });
  }

  function connectSSE() {
    if (!state.token || state.es) return;
    if (typeof window.EventSource !== "function") {
      startPolling();
      return;
    }
    var source;
    try {
      source = new window.EventSource("/api/events?token=" + encodeURIComponent(state.token));
    } catch (err) {
      startPolling();
      scheduleReconnect();
      return;
    }
    state.es = source;

    source.addEventListener("state", function (event) {
      var data;
      try {
        data = JSON.parse(event.data);
      } catch (err) {
        return;
      }
      applyState(data);
    });

    source.onopen = function () {
      if (state.es !== source) return;
      state.retryDelay = 1000;
      stopPolling();
      setConnState("online");
    };

    source.onerror = function () {
      if (state.es === source) state.es = null;
      try {
        source.close();
      } catch (err) {
        /* already closed */
      }
      setConnState("offline");
      startPolling();
      scheduleReconnect();
      fetchState();
    };
  }

  function scheduleReconnect() {
    if (state.retryTimer || !state.token) return;
    var delay = state.retryDelay;
    state.retryDelay = Math.min(state.retryDelay * 2, RETRY_MAX_MS);
    state.retryTimer = window.setTimeout(function () {
      state.retryTimer = null;
      if (state.token) connectSSE();
    }, delay);
  }

  function startPolling() {
    if (state.pollTimer || !state.token) return;
    state.pollTimer = window.setInterval(function () {
      fetchState();
    }, POLL_MS);
  }

  function stopPolling() {
    if (state.pollTimer) {
      window.clearInterval(state.pollTimer);
      state.pollTimer = null;
    }
  }

  function stopStreams() {
    if (state.es) {
      try {
        state.es.close();
      } catch (err) {
        /* ignore */
      }
      state.es = null;
    }
    stopPolling();
    if (state.retryTimer) {
      window.clearTimeout(state.retryTimer);
      state.retryTimer = null;
    }
    state.retryDelay = 1000;
  }

  /* ---------- rendering ---------- */

  function applyState(data) {
    if (!data || typeof data !== "object") return;
    state.lastData = data;
    if (data.locale === "tr" || data.locale === "en") {
      storageSet(LOCALE_KEY, data.locale);
      if (!state.langExplicit && data.locale !== state.lang) {
        state.lang = data.locale;
        applyStaticI18n();
      }
    }
    var panel = $("#connectPanel");
    var main = $("#main");
    if (panel) panel.hidden = true;
    if (main) main.hidden = false;
    setConnState("online");
    render(data);
  }

  function render(data) {
    renderStatus(data);
    renderQueue(data);
    renderTransport(data);
    renderTimers(data);
    renderPrompter(data);
    renderDeck(data);
    renderPatterns(data);
    renderSoundboard(data);
    renderUpload(data);
    renderFooter(data);
  }

  function chip(text, warn) {
    return el("span", "chip" + (warn ? " warn" : ""), text);
  }

  function sourceType(source) {
    if (source == null) return "black";
    if (typeof source === "string") return source;
    if (typeof source === "object") {
      if (typeof source.type === "string") return source.type;
      var keys = Object.keys(source);
      if (keys.length) return keys[0];
    }
    return "black";
  }

  function sourceLabel(source) {
    var kind = sourceType(source);
    var known = t("transport.source." + kind);
    return (t("transport.source") || "Source") + ": " + (known != null ? known : kind);
  }

  function renderStatus(data) {
    var project = data.project;
    setText($("#projectName"), project ? String(project) : (t("status.noProject") || ""));
    var program = data.program && typeof data.program === "object" ? data.program : {};
    setText($("#programName"), program.name ? String(program.name) : (t("status.none") || ""));

    var onAir = !!program.on_air;
    var badge = $("#onAirBadge");
    if (badge) {
      badge.textContent = onAir ? (t("status.onAir") || "ON AIR") : (t("status.offAir") || "OFF AIR");
      badge.classList.toggle("on-air", onAir);
    }

    var chips = $("#statusChips");
    if (chips) {
      chips.replaceChildren();
      if (program.black) chips.append(chip(t("status.black") || "Black", true));
      if (program.freeze) chips.append(chip(t("status.freeze") || "Frozen", true));
      if (program.mute) chips.append(chip(t("status.mute") || "Muted", true));
      if (data.pattern_override) chips.append(chip(t("status.pattern") || "Pattern", true));
    }

    var meta = [];
    if (program.source != null) meta.push(sourceLabel(program.source));
    if (program.remaining_secs != null && Number(program.remaining_secs) > 0) {
      meta.push((t("status.remaining") || "Remaining") + " " + formatClock(program.remaining_secs));
    }
    setText($("#showMeta"), meta.join(" · "));
  }

  function renderQueue(data) {
    var queue = data.queue && typeof data.queue === "object" ? data.queue : {};
    var current = queue.current;
    var box = $("#queueCurrent");
    if (box) {
      box.replaceChildren();
      box.classList.toggle("has-current", !!current);
      if (current) {
        var ticket = el("div", "current-ticket", current.ticket == null ? "" : String(current.ticket));
        ticket.setAttribute("aria-label", t("queue.ticket") || "Ticket");
        box.append(el("span", "label", t("queue.current") || "Now calling"));
        box.append(ticket);
        box.append(el("div", "current-name", current.name == null ? "" : String(current.name)));
        if (current.note) box.append(el("div", "current-note", String(current.note)));
      } else {
        box.append(el("p", "muted small", t("queue.emptyCurrent") || "No one is being called"));
      }
    }

    var stats = t("queue.stats", {
      waiting: num(queue.waiting, 0),
      called: num(queue.called, 0),
      done: num(queue.done, 0),
      skipped: num(queue.skipped, 0)
    });
    setText($("#queueStats"), stats || "");

    var list = $("#queueNext");
    if (!list) return;
    list.replaceChildren();
    var rows = Array.isArray(queue.next) ? queue.next.slice(0, 5) : [];
    if (!rows.length) {
      list.append(el("li", "next-empty", t("queue.emptyNext") || "No one is waiting"));
      return;
    }
    rows.forEach(function (row) {
      if (!row || typeof row !== "object") return;
      var item = el("li", "next-row");
      item.append(el("span", "ticket-chip", row.ticket == null ? "" : String(row.ticket)));
      item.append(el("span", "next-name", row.name == null ? "" : String(row.name)));
      if (row.note) item.append(el("span", "next-note", String(row.note)));
      list.append(item);
    });
  }

  function setPressed(selector, value) {
    $$(selector).forEach(function (btn) {
      btn.setAttribute("aria-pressed", value ? "true" : "false");
    });
  }

  function renderTransport(data) {
    var program = data.program && typeof data.program === "object" ? data.program : {};
    var playlist = data.playlist && typeof data.playlist === "object" ? data.playlist : {};

    setPressed('[data-toggle="black"]', !!program.black);
    setPressed('[data-toggle="freeze"]', !!program.freeze);
    setPressed('[data-toggle="mute"]', !!program.mute);
    setPressed('[data-toggle="auto"]', !!playlist.auto);

    var total = Math.max(0, Math.round(num(playlist.total, 0)));
    var index = Math.round(num(playlist.preview_index, 0));
    if (total > 0) {
      setText($("#playlistMeta"), (t("transport.preview") || "Preview") + " " +
        t("transport.itemOf", { index: clamp(index + 1, 1, total), total: total }));
    } else {
      setText($("#playlistMeta"), "");
    }

    var select = $("#playlistSelect");
    if (select) {
      var options = [];
      if (Array.isArray(data.playlists)) {
        data.playlists.forEach(function (item) {
          if (item && item.id != null) {
            options.push({ id: String(item.id), name: item.name != null ? String(item.name) : String(item.id) });
          }
        });
      }
      if (!options.length && playlist.id != null) {
        options.push({ id: String(playlist.id), name: playlist.name != null ? String(playlist.name) : String(playlist.id) });
      }
      var signature = JSON.stringify(options);
      if (select.getAttribute("data-sig") !== signature) {
        select.replaceChildren();
        options.forEach(function (option) {
          var node = el("option", null, option.name);
          node.value = option.id;
          select.append(node);
        });
        if (!options.length) {
          var empty = el("option", null, t("common.none") || "—");
          empty.value = "";
          select.append(empty);
        }
        select.setAttribute("data-sig", signature);
      }
      if (playlist.id != null && select.value !== String(playlist.id)) {
        select.value = String(playlist.id);
      }
    }

    setText($("#sourceMeta"), program.source != null ? sourceLabel(program.source) : "");
  }

  function timeCell(label, value, running) {
    var wrap = el("span", "timer-time");
    wrap.append(el("span", "label", label || ""));
    wrap.append(el("span", "timer-value" + (running ? " running" : ""), value));
    return wrap;
  }

  function timerButton(action, id, label, disabled) {
    var btn = el("button", "btn", label || "");
    btn.type = "button";
    btn.setAttribute("data-action", action);
    if (id != null) btn.setAttribute("data-id", String(id));
    btn.disabled = !!disabled;
    return btn;
  }

  function renderTimers(data) {
    var list = $("#timerList");
    if (!list) return;
    list.replaceChildren();
    var timers = Array.isArray(data.timers) ? data.timers : [];
    if (!timers.length) {
      list.append(el("p", "muted small", t("timers.none") || ""));
      return;
    }
    timers.forEach(function (timer) {
      if (!timer || typeof timer !== "object") return;
      var running = !!timer.running;
      var row = el("div", "timer-row");
      row.append(el("div", "timer-name", timer.name != null ? String(timer.name) : (timer.id != null ? String(timer.id) : "")));
      var times = el("div", "timer-times");
      times.append(timeCell(t("timers.elapsed") || "Elapsed", formatClock(timer.elapsed_secs), running));
      times.append(timeCell(t("timers.remaining") || "Remaining", formatClock(timer.remaining_secs), running));
      row.append(times);
      var actions = el("div", "btn-grid");
      actions.append(timerButton("timer.start", timer.id, t("timers.start") || "Start", running));
      actions.append(timerButton("timer.pause", timer.id, t("timers.pause") || "Pause", !running));
      actions.append(timerButton("timer.reset", timer.id, t("timers.reset") || "Reset", false));
      row.append(actions);
      list.append(row);
    });
  }

  function renderPrompter(data) {
    var prompter = data.prompter && typeof data.prompter === "object" ? data.prompter : {};
    var badge = $("#prompterState");
    if (badge) {
      badge.textContent = prompter.running ? (t("prompter.running") || "Running") : (t("prompter.paused") || "Paused");
      badge.classList.toggle("good", !!prompter.running);
    }
    var range = $("#speedRange");
    var speed = Number(prompter.speed);
    if (range) {
      if (Number.isFinite(speed)) {
        if (document.activeElement !== range) {
          range.value = String(clamp(Math.round(speed), 10, 200));
        }
        setText($("#speedValue"), String(Math.round(speed)));
      } else {
        setText($("#speedValue"), range.value);
      }
    }
  }

  function renderDeck(data) {
    var deck = data.deck && typeof data.deck === "object" ? data.deck : {};
    var available = !!deck.available;
    setText($("#deckLabel"), deck.label != null ? String(deck.label) : (available ? "" : (t("deck.unavailable") || "")));
    var hint = deck.hint != null ? String(deck.hint) : "";
    var hintLabel = t("deck.hint");
    setText($("#deckHint"), hint ? ((hintLabel ? hintLabel + ": " : "") + hint) : "");

    var backend = $("#deckBackend");
    if (backend) {
      if (deck.backend) {
        var backendText = String(deck.backend);
        var backendLabel = t("deck.backend") || "Backend";
        backend.hidden = false;
        backend.textContent = backendText;
        backend.setAttribute("title", backendLabel + ": " + backendText);
        backend.setAttribute("aria-label", backendLabel + ": " + backendText);
      } else {
        backend.hidden = true;
        backend.textContent = "";
        backend.removeAttribute("title");
        backend.removeAttribute("aria-label");
      }
    }

    var error = deck.last_error ? String(deck.last_error) : "";
    var errorNode = $("#deckError");
    if (errorNode) {
      errorNode.hidden = !error;
      errorNode.textContent = error;
    }
    $$('#deckSection [data-action^="deck."]').forEach(function (btn) {
      btn.disabled = !available;
    });

    if (error && error !== state.lastDeckError) {
      toast(t("deck.error", { error: error }) || error, "error");
    }
    state.lastDeckError = error;
  }

  function renderPatterns(data) {
    var box = $("#patternList");
    if (!box) return;
    var patterns = Array.isArray(data.patterns) ? data.patterns : [];
    var items = [];
    patterns.forEach(function (pattern) {
      if (pattern && pattern.id != null) {
        items.push({ id: String(pattern.id), name: pattern.name != null ? String(pattern.name) : String(pattern.id) });
      }
    });
    var signature = JSON.stringify(items);
    if (box.getAttribute("data-sig") === signature) return;
    box.setAttribute("data-sig", signature);
    box.replaceChildren();
    items.forEach(function (item) {
      var btn = el("button", "btn", item.name);
      btn.type = "button";
      btn.setAttribute("data-action", "pattern.show");
      btn.setAttribute("data-id", item.id);
      box.append(btn);
    });
  }

  function renderSoundboard(data) {
    var soundboard = data.soundboard && typeof data.soundboard === "object" ? data.soundboard : {};
    var pages = Array.isArray(soundboard.pages) ? soundboard.pages : [];
    var signature = JSON.stringify({
      active: soundboard.active_page,
      pages: pages.map(function (page) {
        var pads = Array.isArray(page && page.pads) ? page.pads : [];
        return {
          id: page && page.id != null ? String(page.id) : "",
          name: page && page.name != null ? String(page.name) : "",
          pads: pads.map(function (pad) {
            return {
              id: pad && pad.id != null ? String(pad.id) : "",
              label: pad && pad.label != null ? String(pad.label) : "",
              color: pad && pad.color != null ? String(pad.color) : "",
              hotkey: pad && pad.hotkey != null ? String(pad.hotkey) : "",
              playing: !!(pad && pad.playing)
            };
          })
        };
      })
    });
    if (state.soundSig === signature) return;
    state.soundSig = signature;

    var active = clamp(Math.round(num(soundboard.active_page, 0)), 0, Math.max(pages.length - 1, 0));
    var tabs = $("#pageTabs");
    if (tabs) {
      tabs.replaceChildren();
      tabs.setAttribute("aria-label", t("soundboard.page") || "Page");
      pages.forEach(function (page, index) {
        var tab = el("button", "tab", page && page.name != null ? String(page.name) : String(index + 1));
        tab.type = "button";
        tab.setAttribute("role", "tab");
        tab.setAttribute("aria-selected", index === active ? "true" : "false");
        tab.setAttribute("data-action", "soundboard.page");
        tab.setAttribute("data-index", String(index));
        tabs.append(tab);
      });
    }

    var grid = $("#padGrid");
    if (!grid) return;
    grid.replaceChildren();
    var page = pages[active];
    var pads = page && Array.isArray(page.pads) ? page.pads : [];
    if (!pads.length) {
      grid.append(el("p", "muted small", t("soundboard.empty") || ""));
      return;
    }
    pads.forEach(function (pad) {
      if (!pad || pad.id == null) return;
      var button = el("button", "pad" + (pad.playing ? " playing" : ""));
      button.type = "button";
      button.setAttribute("data-action", pad.playing ? "soundboard.stop" : "soundboard.play");
      button.setAttribute("data-pad-id", String(pad.id));
      button.setAttribute("aria-pressed", pad.playing ? "true" : "false");
      var color = safeColor(pad.color);
      if (color) button.style.backgroundColor = color;
      button.append(el("span", "pad-label", pad.label ? String(pad.label) : (t("soundboard.padEmpty") || "")));
      if (pad.hotkey) button.append(el("span", "pad-hotkey", String(pad.hotkey)));
      grid.append(button);
    });
  }

  function renderUpload(data) {
    var gui = (data.mode || "gui") === "gui";
    var form = $("#uploadForm");
    var blocked = $("#uploadBlocked");
    if (form) form.hidden = !gui;
    if (blocked) blocked.hidden = gui;
  }

  function renderFooter(data) {
    var version = data.remote && typeof data.remote === "object" ? data.remote.version : null;
    if (version == null || version === "") {
      setText($("#versionLabel"), "");
      return;
    }
    setText($("#versionLabel"), t("footer.version", { version: version }) || String(version));
  }

  function renderNoProject() {
    state.lastData = null;
    state.lastDeckError = null;
    var panel = $("#connectPanel");
    var main = $("#main");
    if (panel) panel.hidden = true;
    if (main) main.hidden = false;

    setText($("#projectName"), t("status.noProject") || "");
    setText($("#programName"), t("status.none") || "");
    var badge = $("#onAirBadge");
    if (badge) {
      badge.textContent = t("status.offAir") || "OFF AIR";
      badge.classList.remove("on-air");
    }
    var chips = $("#statusChips");
    if (chips) chips.replaceChildren();
    setText($("#showMeta"), "");

    var current = $("#queueCurrent");
    if (current) {
      current.replaceChildren();
      current.classList.remove("has-current");
      current.append(el("p", "muted small", t("queue.emptyCurrent") || ""));
    }
    var next = $("#queueNext");
    if (next) next.replaceChildren();
    setText($("#queueStats"), "");

    setPressed('[data-toggle="black"]', false);
    setPressed('[data-toggle="freeze"]', false);
    setPressed('[data-toggle="mute"]', false);
    setPressed('[data-toggle="auto"]', false);
    setText($("#playlistMeta"), "");
    setText($("#sourceMeta"), "");
    var select = $("#playlistSelect");
    if (select) {
      select.replaceChildren();
      select.removeAttribute("data-sig");
    }

    var timers = $("#timerList");
    if (timers) {
      timers.replaceChildren();
      timers.append(el("p", "muted small", t("timers.none") || ""));
    }
    var prompterBadge = $("#prompterState");
    if (prompterBadge) {
      prompterBadge.textContent = t("prompter.paused") || "Paused";
      prompterBadge.classList.remove("good");
    }
    setText($("#deckLabel"), "");
    setText($("#deckHint"), "");
    var deckError = $("#deckError");
    if (deckError) deckError.hidden = true;
    var backend = $("#deckBackend");
    if (backend) backend.hidden = true;
    $$('#deckSection [data-action^="deck."]').forEach(function (btn) {
      btn.disabled = true;
    });

    var patterns = $("#patternList");
    if (patterns) {
      patterns.replaceChildren();
      patterns.removeAttribute("data-sig");
    }
    var tabs = $("#pageTabs");
    if (tabs) tabs.replaceChildren();
    var pads = $("#padGrid");
    if (pads) pads.replaceChildren();
    state.soundSig = null;
  }

  /* ---------- actions ---------- */

  function toast(message, kind) {
    var node = $("#toast");
    if (!node || message == null) return;
    node.textContent = String(message);
    node.className = "toast show" + (kind ? " " + kind : "");
    window.clearTimeout(toast.timer);
    toast.timer = window.setTimeout(function () {
      node.className = "toast" + (kind ? " " + kind : "");
    }, 3800);
  }

  function lock(btn) {
    if (!btn || btn.disabled) return;
    btn.disabled = true;
    btn.classList.add("busy");
    window.setTimeout(function () {
      btn.disabled = false;
      btn.classList.remove("busy");
    }, LOCK_MS);
  }

  function send(action, args, btn) {
    if (!state.token) {
      showConnect(null);
      return Promise.resolve(false);
    }
    lock(btn);
    return fetch("/api/actions", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Signifiage-Token": state.token
      },
      body: JSON.stringify({ action: action, args: args || {} })
    })
      .then(function (res) {
        if (res.status === 401) {
          authFailed();
          return false;
        }
        if (res.status === 409) {
          toast(t("toast.noProject") || "No project is open.", "error");
          return false;
        }
        if (!res.ok) {
          toast(t("toast.actionFailed") || "Action failed.", "error");
          return false;
        }
        return true;
      })
      .catch(function () {
        toast(t("toast.offline") || "Not connected.", "error");
        return false;
      });
  }

  function toggleSend(btn, action) {
    var next = btn.getAttribute("aria-pressed") !== "true";
    btn.setAttribute("aria-pressed", next ? "true" : "false");
    return send(action, { on: next }, btn).then(function (ok) {
      if (!ok && state.lastData) render(state.lastData);
      return ok;
    });
  }

  function handleAction(btn) {
    var action = btn.getAttribute("data-action");

    switch (action) {
      case "playback.black":
        toggleSend(btn, action);
        return;
      case "playback.freeze":
        toggleSend(btn, action);
        return;
      case "playback.mute":
        toggleSend(btn, action);
        return;
      case "playback.auto":
        toggleSend(btn, action);
        return;
      case "playback.playlist":
        if (btn.value) send(action, { id: btn.value }, btn);
        return;
      case "queue.add":
        return;
      case "timer.start":
      case "timer.pause":
      case "timer.reset":
        send(action, { id: btn.getAttribute("data-id") || "" }, btn);
        return;
      case "soundboard.play":
      case "soundboard.stop":
        send(action, { pad_id: btn.getAttribute("data-pad-id") || "" }, btn);
        return;
      case "soundboard.page":
        send(action, { index: Math.round(num(btn.getAttribute("data-index"), 0)) }, btn);
        return;
      case "pattern.show":
        send(action, { id: btn.getAttribute("data-id") || "" }, btn);
        return;
      case "prompter.speed":
        send(action, { value: Math.round(num(btn.value, 45)) }, btn);
        return;
      case "queue.next":
      case "queue.recall":
      case "queue.done":
      case "queue.skip":
      case "queue.undo":
      case "playback.next":
      case "playback.prev":
      case "playback.go":
      case "prompter.start":
      case "prompter.pause":
      case "prompter.reset":
      case "soundboard.stop_all":
      case "deck.next":
      case "deck.prev":
      case "pattern.resume":
        send(action, {}, btn);
        return;
      default:
        return;
    }
  }

  function readTokenFromUrl() {
    var url;
    try {
      url = new URL(window.location.href);
    } catch (err) {
      return null;
    }
    var token = url.searchParams.get("token");
    if (!token) return null;
    url.searchParams.delete("token");
    var query = url.searchParams.toString();
    var clean = url.pathname + (query ? "?" + query : "") + url.hash;
    try {
      window.history.replaceState(null, "", clean);
    } catch (err) {
      /* file:// and sandboxed contexts may refuse; the token is still used. */
    }
    return token;
  }

  function wire() {
    var connectForm = $("#connectForm");
    if (connectForm) {
      connectForm.addEventListener("submit", function (event) {
        event.preventDefault();
        var input = $("#tokenInput");
        var value = input ? input.value.trim() : "";
        if (!value) {
          var error = $("#connectError");
          if (error) {
            error.hidden = false;
            error.textContent = t("connect.tokenRequired") || "Enter the remote token first.";
          }
          if (input) input.focus();
          return;
        }
        state.token = value;
        storageSet(TOKEN_KEY, value);
        var errorNode = $("#connectError");
        if (errorNode) {
          errorNode.hidden = true;
          errorNode.textContent = "";
        }
        start();
      });
    }

    document.addEventListener("click", function (event) {
      var target = event.target;
      if (!target || typeof target.closest !== "function") return;
      var langBtn = target.closest(".lang-btn");
      if (langBtn) {
        setLang(langBtn.getAttribute("data-lang"), true);
        return;
      }
      var actionNode = target.closest("[data-action]");
      if (!actionNode) return;
      event.preventDefault();
      handleAction(actionNode);
    });

    var queueForm = $("#queueAddForm");
    if (queueForm) {
      queueForm.addEventListener("submit", function (event) {
        event.preventDefault();
        var nameInput = $("#queueName");
        var noteInput = $("#queueNote");
        var name = nameInput ? nameInput.value.trim() : "";
        var note = noteInput ? noteInput.value.trim() : "";
        if (!name) {
          toast(t("queue.nameRequired") || "Enter a name first.", "error");
          if (nameInput) nameInput.focus();
          return;
        }
        var submit = queueForm.querySelector('button[type="submit"]');
        send("queue.add", { name: name, note: note }, submit).then(function (ok) {
          if (!ok) return;
          if (nameInput) nameInput.value = "";
          if (noteInput) noteInput.value = "";
        });
      });
    }

    var playlistSelect = $("#playlistSelect");
    if (playlistSelect) {
      playlistSelect.addEventListener("change", function () {
        var id = playlistSelect.value;
        if (!id) return;
        send("playback.playlist", { id: id }, null).then(function (ok) {
          if (!ok && state.lastData) render(state.lastData);
        });
      });
    }

    var speedRange = $("#speedRange");
    if (speedRange) {
      speedRange.addEventListener("input", function () {
        setText($("#speedValue"), speedRange.value);
      });
      speedRange.addEventListener("change", function () {
        var value = clamp(Math.round(num(speedRange.value, 45)), 10, 200);
        send("prompter.speed", { value: value }, null);
      });
    }

    var uploadForm = $("#uploadForm");
    if (uploadForm) {
      uploadForm.addEventListener("submit", function (event) {
        event.preventDefault();
        var input = $("#uploadInput");
        var file = input && input.files && input.files[0];
        if (!file) {
          toast(t("upload.noFile") || "Choose a file first.", "error");
          return;
        }
        var submit = uploadForm.querySelector('button[type="submit"]');
        if (submit) {
          submit.disabled = true;
          submit.classList.add("busy");
        }
        var body = new FormData();
        body.append("file", file, file.name);
        fetch("/api/upload", {
          method: "POST",
          headers: { "X-Signifiage-Token": state.token || "" },
          body: body
        })
          .then(function (res) {
            if (res.status === 401) {
              authFailed();
              return null;
            }
            if (res.status === 409) {
              toast(t("toast.noProject") || "No project is open.", "error");
              return null;
            }
            if (res.status === 415) {
              toast(t("upload.unsupported") || "That file type is not allowed.", "error");
              return null;
            }
            if (res.status === 413) {
              toast(t("upload.tooLarge") || "That file is too large.", "error");
              return null;
            }
            if (!res.ok) {
              toast(t("upload.failed") || "Upload failed.", "error");
              return null;
            }
            return res.json().catch(function () {
              return null;
            });
          })
          .then(function (payload) {
            if (!payload) return;
            var asset = payload.asset != null ? String(payload.asset) : "";
            if (!asset) {
              toast(t("upload.failed") || "Upload failed.", "error");
              return;
            }
            toast(t("upload.done", { asset: asset }) || asset, "success");
            if (input) input.value = "";
          })
          .catch(function () {
            toast(t("toast.offline") || "Not connected.", "error");
          })
          .then(function () {
            if (submit) {
              submit.disabled = false;
              submit.classList.remove("busy");
            }
          });
      });
    }
  }

  function init() {
    state.lang = preferredLang();
    state.langExplicit = storageGet(LANG_KEY) === "en" || storageGet(LANG_KEY) === "tr";
    var urlToken = readTokenFromUrl();
    if (urlToken) {
      state.token = urlToken;
      storageSet(TOKEN_KEY, urlToken);
    } else {
      state.token = storageGet(TOKEN_KEY) || null;
    }

    wire();

    var onFile = window.location.protocol === "file:";
    var ready = onFile ? Promise.resolve() : loadI18n();
    return ready.then(function () {
      applyStaticI18n();
      if (onFile) {
        showConnect(null);
        return;
      }
      if (state.token) {
        start();
      } else {
        showConnect(null);
      }
    });
  }

  init().catch(function () {
    showConnect(null);
  });
})();
