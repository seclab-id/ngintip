import { listen } from '@tauri-apps/api/event';
import { open, save } from '@tauri-apps/plugin-dialog';
import * as api from './api.js';
import { DEFAULT_COLUMN_KEYS, normalizeColumns } from './columns.js';
import { emptyAdvanced, advActive, newFilter, BASIC_EXCLUDE_FILTERS } from './histfilter.js';
import { emptyHide } from './hidefilter.js';
import { scanFlow, compileRules, ruleError, SEVERITY_ORDER, flowInScope, DEFAULT_PASSIVE_SCOPE } from './passive.js';
import { inScope } from './scope.js';
import { BUILTIN_RULES } from './passive-rules.js';
import { isLoopback } from './net.js';
import { toCurl } from './curl.js';
import { generate, MAX_REQUESTS, defaultPayloadConfig } from './intruder.js';
import { followRequest, redirectInfo } from './redirect.js';
import { BRAND, brandKey } from './brand.js';
import { newInterceptRule, normalizeInterceptRule } from './interceptfilter.js';

// Extension scope surfaces (must match Rust's Surface tokens), in canonical order.
export const EXT_SURFACES = ['proxy', 'interceptor', 'repeater'];
/** Parse an extension scope string into the set of surfaces it covers. Empty, "all"
 *  and the legacy "both" all mean every surface. */
export function extScopeSet(scope) {
  const s = String(scope ?? '').trim().toLowerCase();
  if (s === '' || s === 'all' || s === 'both') return new Set(EXT_SURFACES);
  return new Set(s.split(/[\s,|+]+/).filter(Boolean));
}
/** Serialize a surface set back to a scope string ("all" when every surface is on). */
export function extScopeString(set) {
  const on = EXT_SURFACES.filter((x) => set.has(x));
  return on.length === EXT_SURFACES.length ? 'all' : on.join(',');
}

// Repeater tabs live in the project file (settings table) under this key.
function loadTheme() {
  try {
    return localStorage.getItem(brandKey('theme')) || 'system';
  } catch {
    return 'system';
  }
}

// Opened-flow detail cache ceiling, persisted per machine (a UI memory/latency knob).
function loadDetailCacheMax() {
  try {
    const n = Number(localStorage.getItem(brandKey('detailCache')));
    return Number.isFinite(n) && n >= 5 ? Math.round(n) : DETAIL_CACHE_MAX;
  } catch {
    return DETAIL_CACHE_MAX;
  }
}

// Resolve 'system' to the live OS preference, so the stylesheet only needs ONE light
// block (:root[data-theme='light']) instead of duplicating it in a prefers-color-scheme
// media query. The result is always written to <html data-theme>.
function resolveTheme(theme) {
  if (theme !== 'system') return theme;
  try {
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

function applyTheme(theme) {
  if (typeof document === 'undefined') return;
  document.documentElement.setAttribute('data-theme', resolveTheme(theme));
}

// Follow the OS appearance live while the user is on 'system'.
if (typeof window !== 'undefined' && window.matchMedia) {
  try {
    window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => {
      if (loadTheme() === 'system') applyTheme('system');
    });
  } catch {
    /* older webviews: ignore */
  }
}

const REPEATER_SETTING = 'repeater';
const HISTORY_COLUMNS_SETTING = 'historyColumns';
const HISTORY_SORT_SETTING = 'historySort';
const HISTORY_HIDE_SETTING = 'historyHide';
// "In scope only" checkbox + the advanced (structured) History filter, per project.
const HISTORY_FILTER_SETTING = 'historyFilter';
// Auto-save config (enabled + interval seconds), per project.
const AUTOSAVE_SETTING = 'autoSave';
// Passive-scan config, per project: custom rules and which built-ins are off.
const PASSIVE_RULES_SETTING = 'passiveRules';
const PASSIVE_DISABLED_SETTING = 'passiveDisabled';
const PASSIVE_FINDINGS_SETTING = 'passiveFindings';
const PASSIVE_SCANNED_SETTING = 'passiveScanned';
const PASSIVE_SCOPE_SETTING = 'passiveScope';
const PASSIVE_INSCOPE_SETTING = 'passiveInScope';
const PASSIVE_COLLAPSED_SETTING = 'passiveCollapsed';
const INTERCEPT_FILTER_SETTING = 'interceptFilter';
// Chrome DevTools Protocol for the launched browser, saved per project (default off).
const BROWSER_CDP_SETTING = 'browserCdp';
// Persistent browser profiles (cookies/history next to the project) + multi-profile list.
const BROWSER_PROFILES_SETTING = 'browserProfiles';
// Where tabs were kept before they moved into the project (webview-wide).
const LEGACY_REPEATER_KEY = 'intip.repeater';

function makeTab(id, fields = {}) {
  return {
    id,
    name: String(id),
    group: '',
    target: 'https://example.com',
    raw: DEFAULT_RAW,
    note: '',
    response: null,
    sending: false,
    error: null,
    // Sent requests of this tab (oldest first, from the project file), the one
    // shown (index, or null while editing freely) and unsent edits set aside
    // while browsing the history.
    history: [],
    historyLoaded: false,
    pos: null,
    draft: null,
    loadingEntry: false,
    ...fields,
  };
}

/** Tabs saved by older versions in localStorage, removed once read. */
function takeLegacyRepeater() {
  try {
    const saved = JSON.parse(localStorage.getItem(LEGACY_REPEATER_KEY));
    localStorage.removeItem(LEGACY_REPEATER_KEY);
    return saved;
  } catch {
    return null;
  }
}

/** Only name/target/raw are worth keeping; responses are re-sent on demand. */
function repeaterSnapshot({ tabs, activeId, groupColors, collapsedGroups }) {
  return { activeId, groupColors: { ...groupColors }, collapsedGroups: [...(collapsedGroups ?? [])], tabs: tabs.map(({ id, name, group, target, raw, note }) => ({ id, name, group, target, raw, note: note || '' })) };
}

const DEFAULT_RAW = `GET / HTTP/1.1
Host: example.com
User-Agent: ${BRAND.userAgent}
Accept: */*

`;

const MARK = '\u00a7';
const DEFAULT_INTRUDER_RAW = `GET /search?q=${MARK}test${MARK} HTTP/1.1\nHost: example.com\nUser-Agent: ${BRAND.userAgent}\nAccept: */*\n\n`;
// Keep full responses only for the first rows of a run, to bound memory.
const KEEP_RESPONSES = 500;
const RESPONSE_CAP = 200 * 1024;

function makeIntruderTab(id, fields = {}) {
  return {
    id,
    name: String(id),
    target: 'https://example.com',
    raw: DEFAULT_INTRUDER_RAW,
    type: 'sniper',
    sets: [defaultPayloadConfig()],
    concurrency: 10,
    running: false,
    results: [],
    total: 0,
    sent: 0,
    error: '',
    selected: null,
    ...fields,
  };
}

// How many opened-flow details to keep cached (see loadDetail). Each entry is a
// get_flow result (capped request/response text + image data URL, plus up to a few
// variant copies), so this is the LRU ceiling on detail memory. Arrow-key navigation
// only prefetches +/-2 neighbours, so a few dozen keeps nav instant while bounding
// worst-case memory instead of letting it grow unbounded.
export const DETAIL_CACHE_MAX = 45;

class Store {
  view = $state('history');
  // Startup project chooser is shown until the user picks a project.
  chooser = $state(true);
  // 'system' | 'light' | 'dark'; applied to <html data-theme>.
  theme = $state(loadTheme());
  // Opened-flow detail cache ceiling (configurable in Settings > Project).
  detailCacheMax = $state(loadDetailCacheMax());

  project = $state({ name: '', path: '' });
  scope = $state.raw([]);
  scopeOnly = $state(false);
  // Match & Replace rules of the open project.
  rules = $state.raw([]);

  proxy = $state({ running: false, host: '127.0.0.1', port: 8080 });
  proxyError = $state(null);
  notice = $state(null);
  // True while a compressed .cegatc project is being decompressed (shows overlay).
  decompressing = $state(false);
  // True while the project is being snapshotted + compressed to disk (shows overlay).
  saving = $state(false);
  // App-wide settings (src-tauri/src/config.rs), shared by every project.
  config = $state({ auth: { enabled: false, username: '', password: '' }, upstream: '', invisible: false, autoContentLength: true, mcp: false, mcpPort: 8082, mcpDocsPath: '/docs' });
  // MCP server (lets an LLM drive the app over HTTP).
  mcp = $state({ running: false, port: 8082, docsPath: '/docs' });
  // Browser engine: a Chromium binary the History "Open browser" button launches
  // wired to the proxy. Downloading state drives the Settings progress UI.
  browser = $state({ path: '', exists: false, flags: '', canDownload: true, downloading: false, downloadPct: null, downloadError: '', opened: false });
  // Chrome DevTools Protocol for the launched browser (per project; default off). When
  // on, the browser is launched with --remote-debugging-port=<port> on 127.0.0.1.
  cdp = $state({ enabled: false, port: 9222 });
  // Browser profiles (per project): `persist` saves the default profile's cookies/history
  // next to the .ngintip; `multi` + `list` give named profiles you can spawn separately.
  profiles = $state({ persist: false, multi: false, list: [] });
  // Lets other views deep-link to a Settings sub-tab (e.g. 'browser').
  settingsSection = $state(null);
  // Auto-save the project every `interval` seconds when enabled (per project).
  // Default: on, every hour (3600s). Saved into the project, so each project keeps its own.
  autoSave = $state({ enabled: true, interval: 3600 });
  // Python extensions: engine on/off, interpreter, venv, loaded scripts, setup log.
  extensions = $state({ enabled: false, python: '', venv: '', venvReady: false, list: [], busy: false, log: '' });

  /** Listening where other machines can reach the proxy. */
  get exposed() {
    return this.proxy.running && !isLoopback(this.proxy.host);
  }

  // Raw (non-deep) state: flows are replaced, never mutated in place.
  flows = $state.raw([]);
  selectedId = $state(null);
  // Extra rows picked with ⌘/⇧-click; always includes selectedId when non-empty.
  selectedIds = $state.raw(new Set());
  filter = $state('');
  // Advanced (structured) History filter and whether its panel is open.
  advOpen = $state(false);
  adv = $state(emptyAdvanced());
  // IDs matched by a body search, or null when not searching bodies.
  bodyHits = $state.raw(null);
  searching = $state(false);
  #searchTimer = null;
  // Sitemap pane in History, and the tree node filtering the table (scheme://host/path).
  sitemapOpen = $state(false);
  sitemapNode = $state(null);
  // Which History columns are visible (per project; see columns.js).
  historyColumns = $state.raw([...DEFAULT_COLUMN_KEYS]);
  // History table sort (persisted per project). dir: 1 asc, -1 desc.
  historySort = $state({ key: 'id', dir: 1 });
  // Hide-by-type filter (per project): categories + custom extensions to hide.
  // Its UI lives in the Advanced panel; the filter always applies.
  historyHide = $state(emptyHide());

  // Passive scan (Burp-style): findings from captured traffic, scanned on demand.
  passive = $state({ scanning: false, done: 0, total: 0 });
  passiveFindings = $state.raw([]);
  #passiveScanned = new Set();
  // Opened-flow detail cache (get_flow results), keyed by id. A Map keeps
  // insertion order so the oldest entry is the LRU eviction target. Concurrent
  // loads for the same id share one in-flight promise.
  #detailCache = new Map();
  #detailInflight = new Map();
  // Built-in regex rules (from passive-rules.js) plus user-added custom rules.
  builtinRules = BUILTIN_RULES;
  // Custom rules added in this project, and built-in rule ids turned off.
  passiveRules = $state.raw([]);
  passiveDisabled = $state.raw([]);
  // Which resources to scan, by extension/content-type (['*'] = everything).
  passiveScope = $state.raw([...DEFAULT_PASSIVE_SCOPE]);
  // Restrict the passive scan to flows whose host is in the project Scope.
  passiveInScopeOnly = $state(false);
  // Finding groups the user collapsed (by `${host}${path}` key). Persisted per
  // project so the expanded/collapsed layout survives view switches and restarts.
  passiveCollapsed = $state.raw([]);

  // Decoder view keeps its text (and its multi-step recipe) while switching views.
  decoder = $state({ input: '', output: '', last: null, recipe: [] });
  // Process log (error/warn/info/debug), newest last; unread error/warn count.
  logs = $state.raw([]);
  logsUnread = $state(0);

  // Repeater tabs of the open project. Only name/target/raw are persisted (see saveRepeater).
  repeater = $state({ tabs: [makeTab(1)], activeId: 1, groupColors: {}, collapsedGroups: [] });

  // Intruder attack tabs (in memory for the session).
  intruder = $state({ tabs: [makeIntruderTab(1)], activeId: 1 });

  // Held messages (oldest first), keyed by `key`; a request and its response share `id`.
  // `draft` is the editable copy of the selected one.
  intercept = $state({ enabled: false, responses: false, queue: [], selectedKey: null, draft: '', filter: { rules: [] } });
  // WebSocket intercept: held data messages awaiting Forward/Drop. `draft` edits the
  // selected one's text payload.
  wsIntercept = $state({ enabled: false, queue: [], selectedKey: null, draft: '' });

  #pending = [];
  #wsListeners = new Set();
  #repeaterSaved = '';
  #repeaterTimer = null;
  // Debounced autosave of the History scope + advanced filter (per project).
  #historyFilterSaved = '';
  #historyFilterTimer = null;
  // Periodic project auto-save timer (see autoSave / setAutoSave).
  #autoSaveTimer = null;

  /** Call `fn` for each live WebSocket message; returns an unsubscribe function. */
  onWsMessage(fn) {
    this.#wsListeners.add(fn);
    return () => this.#wsListeners.delete(fn);
  }

  init() {
    applyTheme(this.theme);
    let unlisten;
    (async () => {
      this.project = await api.projectInfo();
      await this.#loadRepeater();
      await this.#loadProjectSettings();
      window.addEventListener('beforeunload', () => {
        this.#flushRepeater();
        this.#flushHistoryFilter();
      });
      this.config = await api.getConfig();
      this.mcp = await api.mcpStatus().catch(() => this.mcp);
      this.browser = { ...this.browser, ...(await api.browserStatus().catch(() => ({}))) };
      this.scope = await api.getScope();
      this.rules = await api.getRules();
      this.flows = await api.listFlows();
      const unlistenFlow = await listen('flow', (e) => this.#enqueue(e.payload));
      const unlistenHeld = await listen('intercept', (e) => this.#onHeld(e.payload));
      const unlistenWs = await listen('ws', (e) => this.#wsListeners.forEach((fn) => fn(e.payload)));
      const unlistenWsHeld = await listen('ws-intercept', (e) => this.#onWsHeld(e.payload));
      const unlistenMenu = await listen('menu', (e) => this.#onMenu(e.payload));
      const unlistenLog = await listen('log', (e) => this.#onLog(e.payload));
      const unlistenDl = await listen('browser-download', (e) => this.#onBrowserDownload(e.payload));
      this.logs = await api.getLogs().catch(() => []);
      unlisten = () =>
        (unlistenFlow(), unlistenHeld(), unlistenWs(), unlistenWsHeld(), unlistenMenu(), unlistenLog(), unlistenDl());

      const held = await api.interceptStatus();
      this.intercept.enabled = held.enabled;
      this.intercept.responses = held.responses;
      held.pending.forEach((item) => this.#onHeld(item));

      const wsHeld = await api.wsInterceptStatus().catch(() => null);
      if (wsHeld) {
        this.wsIntercept.enabled = wsHeld.enabled;
        wsHeld.pending.forEach((item) => this.#onWsHeld(item));
      }

      const status = await api.proxyStatus();
      if (status) this.proxy = status;
      else await this.startProxy();

      // Passive-scan any not-yet-scanned flows in the background (trackable via
      // the progress bar + sidebar badge). Only new flows are processed.
      setTimeout(() => this.scanPassive(), 800);
    })();
    return () => unlisten?.();
  }

  // Batch flow events into one update per animation frame.
  #enqueue(flow) {
    this.#pending.push(flow);
    if (this.#pending.length === 1) requestAnimationFrame(() => this.#flush());
  }

  // A flow can arrive twice (preview while streaming, then final): replace by id.
  #flush() {
    const incoming = new Map(this.#pending.map((f) => [f.id, f]));
    this.#pending = [];
    // A re-arrived flow (streaming preview -> final body) may have new content,
    // so drop any cached detail for it before it's shown again.
    this.#invalidateDetails(incoming.keys());
    const next = this.flows.map((f) => {
      const update = incoming.get(f.id);
      if (!update) return f;
      incoming.delete(f.id);
      return update;
    });
    this.flows = incoming.size ? next.concat([...incoming.values()]) : next;
  }

  async startProxy(host = this.proxy.host, port = this.proxy.port) {
    this.proxyError = null;
    try {
      this.proxy = await api.startProxy(host, Number(port));
    } catch (e) {
      this.proxy = { running: false, host, port };
      this.proxyError = String(e);
    }
  }

  async stopProxy() {
    await api.stopProxy();
    this.proxy = { ...this.proxy, running: false };
  }

  async clear() {
    await api.clearFlows();
    this.flows = [];
    this.selectedId = null;
    this.selectedIds = new Set();
    this.bodyHits = null;
    this.#detailCache.clear();
    this.#detailInflight.clear();
  }

  async newProject() {
    const path = await save({
      title: 'New project',
      defaultPath: `untitled.${BRAND.ext}`,
      filters: [{ name: `${BRAND.name} project`, extensions: [BRAND.ext] }],
    });
    if (!path) return;
    // A new project's file does not exist yet, so it can't be opened (that reads
    // from disk and fails, leaving us stuck on the chooser). Instead start a
    // fresh blank project - which loads state and switches to the main view -
    // then write it to the chosen path so it gains a document on disk.
    await this.blankProject();
    await this.#runSave(() => api.saveProjectAs(path));
  }

  async openProject() {
    const path = await open({
      title: 'Open project',
      multiple: false,
      directory: false,
      filters: [{ name: `${BRAND.name} project`, extensions: BRAND.openExts }],
    });
    if (path) await this.#switchProject(path);
  }

  chooseContinue() {
    this.chooser = false;
  }

  async blankProject() {
    await this.#flushRepeater();
    await this.#flushHistoryFilter();
    this.project = await api.blankProject();
    await this.#loadRepeater();
    await this.#loadProjectSettings();
    this.scope = await api.getScope();
    this.rules = await api.getRules();
    this.#pending = [];
    this.#detailCache.clear();
    this.#detailInflight.clear();
    this.selectedId = null;
    this.selectedIds = new Set();
    this.bodyHits = null;
    this.sitemapNode = null;
    this.flows = await api.listFlows();
    this.logs = await api.getLogs().catch(() => []);
    this.logsUnread = 0;
    this.chooser = false;
  }

  async #switchProject(path) {
    // Finish writing this project's tabs before the file changes underneath.
    await this.#flushRepeater();
    await this.#flushHistoryFilter();
    // A .cegat is compressed; show the overlay while it's expanded and opened.
    this.decompressing = true;
    try {
      // Let the overlay paint before the (blocking) backend work runs.
      await new Promise((r) => setTimeout(r, 60));
      this.project = await api.openProject(path);
      await this.#loadRepeater();
      await this.#loadProjectSettings();
      this.scope = await api.getScope();
      this.rules = await api.getRules();
      this.#pending = [];
      this.#detailCache.clear();
      this.#detailInflight.clear();
      this.selectedId = null;
      this.selectedIds = new Set();
      this.bodyHits = null;
      this.sitemapNode = null;
      this.flows = await api.listFlows();
      this.logs = await api.getLogs().catch(() => []);
      this.logsUnread = 0;
      this.chooser = false;
    } catch (e) {
      this.#notify(String(e), true);
    } finally {
      this.decompressing = false;
    }
  }

  #onHeld(item) {
    const it = this.intercept;
    if (it.queue.some((q) => q.key === item.key)) return;
    it.queue.push(item);
    if (it.selectedKey == null) this.selectHeld(item.key);
  }

  selectHeld(key) {
    const item = this.intercept.queue.find((q) => q.key === key);
    this.intercept.selectedKey = item?.key ?? null;
    this.intercept.draft = item?.raw ?? '';
  }

  async toggleInterceptResponses() {
    const on = !this.intercept.responses;
    await api.setInterceptResponses(on);
    this.intercept.responses = on;
  }

  async toggleIntercept() {
    const enabled = !this.intercept.enabled;
    await api.setIntercept(enabled);
    this.intercept.enabled = enabled;
    // Disabling releases everything on the backend side.
    if (!enabled) {
      this.intercept.queue = [];
      this.selectHeld(null);
    }
  }

  /** Forward the selected message; `holdResponse` also holds the response to a request. */
  async forwardHeld({ holdResponse = false } = {}) {
    const it = this.intercept;
    const item = it.queue.find((q) => q.key === it.selectedKey);
    if (!item) return;
    await api.interceptForward(item.key, it.draft === item.raw ? null : it.draft, holdResponse && item.phase === 'request');
    this.#removeHeld(item.key);
  }

  async dropHeld() {
    const key = this.intercept.selectedKey;
    if (key == null) return;
    await api.interceptDrop(key);
    this.#removeHeld(key);
  }

  async forwardAllHeld() {
    const keys = this.intercept.queue.map((q) => q.key);
    await Promise.all(keys.map((key) => api.interceptForward(key)));
    this.intercept.queue = this.intercept.queue.filter((q) => !keys.includes(q.key));
    this.selectHeld(this.intercept.queue[0]?.key ?? null);
  }

  #removeHeld(key) {
    const it = this.intercept;
    const i = it.queue.findIndex((q) => q.key === key);
    if (i === -1) return;
    it.queue.splice(i, 1);
    this.selectHeld((it.queue[i] ?? it.queue[i - 1])?.key ?? null);
  }

  // ---- WebSocket intercept (mirrors the HTTP intercept queue above) ----
  #onWsHeld(item) {
    const it = this.wsIntercept;
    if (it.queue.some((q) => q.key === item.key)) return;
    it.queue.push(item);
    if (it.selectedKey == null) this.selectWsHeld(item.key);
  }

  selectWsHeld(key) {
    const item = this.wsIntercept.queue.find((q) => q.key === key);
    this.wsIntercept.selectedKey = item?.key ?? null;
    this.wsIntercept.draft = item?.text ?? '';
  }

  async toggleWsIntercept() {
    const enabled = !this.wsIntercept.enabled;
    await api.setWsIntercept(enabled);
    this.wsIntercept.enabled = enabled;
    if (!enabled) {
      this.wsIntercept.queue = [];
      this.selectWsHeld(null);
    }
  }

  /** Forward the selected WebSocket message; edited text is sent unless unchanged. */
  async forwardWsHeld() {
    const it = this.wsIntercept;
    const item = it.queue.find((q) => q.key === it.selectedKey);
    if (!item) return;
    await api.wsInterceptForward(item.key, it.draft === item.text ? null : it.draft);
    this.#removeWsHeld(item.key);
  }

  async dropWsHeld() {
    const key = this.wsIntercept.selectedKey;
    if (key == null) return;
    await api.wsInterceptDrop(key);
    this.#removeWsHeld(key);
  }

  async forwardAllWsHeld() {
    const keys = this.wsIntercept.queue.map((q) => q.key);
    await Promise.all(keys.map((key) => api.wsInterceptForward(key)));
    this.wsIntercept.queue = this.wsIntercept.queue.filter((q) => !keys.includes(q.key));
    this.selectWsHeld(this.wsIntercept.queue[0]?.key ?? null);
  }

  #removeWsHeld(key) {
    const it = this.wsIntercept;
    const i = it.queue.findIndex((q) => q.key === key);
    if (i === -1) return;
    it.queue.splice(i, 1);
    this.selectWsHeld((it.queue[i] ?? it.queue[i - 1])?.key ?? null);
  }

  // The single History search matches summary fields (path/method/status/host…)
  // immediately and, debounced, also full-text searches request/response bodies —
  // the two are unioned in the table. Called on every keystroke.
  searchFilter() {
    clearTimeout(this.#searchTimer);
    this.bodyHits = null; // summary matches show right away; body matches catch up
    const q = this.filter.trim();
    if (!q) {
      this.searching = false;
      return;
    }
    this.searching = true;
    this.#searchTimer = setTimeout(() => this.searchBodies(), 300);
  }

  async searchBodies() {
    clearTimeout(this.#searchTimer);
    const q = this.filter.trim();
    if (!q) {
      this.searching = false;
      return;
    }
    this.searching = true;
    try {
      const hits = new Set(await api.searchFlows(q));
      // Ignore stale results if the query changed while the search was in flight.
      if (this.filter.trim() === q) this.bodyHits = hits;
    } catch {
      /* leave summary matches in place */
    } finally {
      if (this.filter.trim() === q) this.searching = false;
    }
  }

  async saveUpstream(url) {
    this.config.upstream = await api.setUpstream(url);
  }

  async setInvisible(enabled) {
    await api.setInvisible(enabled);
    this.config.invisible = enabled;
  }

  async setAutoContentLength(enabled) {
    await api.setAutoContentLength(enabled);
    this.config.autoContentLength = enabled;
  }

  /** Persist the fixed worker-thread count (0 = auto / one per core). Applies next launch. */
  async setThreads(threads) {
    const n = Math.max(0, Math.min(256, Math.round(Number(threads) || 0)));
    await api.setThreads(n);
    this.config.threads = n;
  }

  async setMcp(enabled, port, docsPath) {
    this.mcp = await api.setMcp(enabled, Number(port) || 8082, docsPath || '/docs');
    this.config.mcp = enabled;
    this.config.mcpPort = this.mcp.port;
    this.config.mcpDocsPath = this.mcp.docsPath;
  }

  // ---- browser engine -------------------------------------------------------

  #mergeBrowser(status) {
    if (status) this.browser = { ...this.browser, ...status };
  }

  async setBrowserPath(path, flags) {
    try {
      this.#mergeBrowser(await api.setBrowser(path ?? '', flags ?? ''));
    } catch (e) {
      this.#notify(String(e), true);
    }
  }

  async detectBrowser() {
    try {
      this.#mergeBrowser(await api.detectBrowser());
      this.#notify(this.browser.exists ? `Found a browser: ${this.browser.path}` : 'No installed browser found', !this.browser.exists);
    } catch (e) {
      this.#notify(String(e), true);
    }
  }

  async downloadBrowser() {
    if (this.browser.downloading) return;
    this.browser = { ...this.browser, downloading: true, downloadPct: 0, downloadError: '' };
    try {
      this.#mergeBrowser(await api.downloadBrowser());
      this.#notify('Browser downloaded and ready');
    } catch (e) {
      this.browser = { ...this.browser, downloadError: String(e) };
      this.#notify(String(e), true);
    } finally {
      this.browser = { ...this.browser, downloading: false };
    }
  }

  #onBrowserDownload(p) {
    if (!p) return;
    if (p.error) this.browser = { ...this.browser, downloadError: p.error, downloading: false };
    else if (p.done) this.browser = { ...this.browser, downloadPct: 100 };
    else this.browser = { ...this.browser, downloadPct: p.pct ?? this.browser.downloadPct };
  }

  /** Launch the pentest browser (from the History toolbar). Ensures the proxy is
   *  running; if no browser is configured, sends the user to set one up. */
  async openPentestBrowser(url, profile) {
    if (!this.browser.exists) {
      this.#notify('Set up a browser first (Settings > Browser Engine)', true);
      this.settingsSection = 'browser';
      this.view = 'settings';
      return;
    }
    if (!this.proxy.running) await this.startProxy();
    const host = ['0.0.0.0', '::', ''].includes(this.proxy.host) ? '127.0.0.1' : this.proxy.host;
    // A named profile is explicit; otherwise use the persistent default when enabled,
    // else a throwaway profile (null).
    const prof = profile !== undefined ? profile : this.profiles.persist ? 'default' : null;
    try {
      const cdpPort = this.cdp.enabled ? this.cdp.port : null;
      await api.launchBrowser(`http://${host}:${this.proxy.port}`, url ?? null, cdpPort, prof);
      this.browser = { ...this.browser, opened: true };
      this.#notify(prof ? `Browser launched (profile "${prof}") - traffic recorded` : 'Browser launched - its traffic is now recorded');
    } catch (e) {
      this.#notify(String(e), true);
    }
  }

  async saveAuth(auth) {
    await api.setAuth(auth);
    this.config.auth = { ...auth };
  }

  async saveRules(rules) {
    await api.setRules(rules);
    this.rules = rules;
  }

  /** Add one host pattern to the project scope (no-op if already present). */
  async addScope(pattern) {
    if (!pattern || this.scope.includes(pattern)) return;
    await this.saveScope([...this.scope, pattern]);
  }

  async saveScope(patterns) {
    this.scope = await api.setScope(patterns);
  }

  /** How many History flows fall OUTSIDE the current project scope (0 when the scope is
   *  empty, since an empty scope means everything is in scope). */
  outOfScopeCount() {
    if (!this.scope.length) return 0;
    let n = 0;
    for (const f of this.flows) if (!inScope(this.scope, f.host)) n++;
    return n;
  }

  /** Delete every History flow whose host is NOT in the current (saved) project scope.
   *  No-op when the scope is empty. When `resetIds` is set, the flow-id counter is reseeded
   *  afterwards so numbering compacts. Returns how many were deleted. */
  async deleteOutOfScope(resetIds = false) {
    if (!this.scope.length) return 0;
    const ids = this.flows.filter((f) => !inScope(this.scope, f.host)).map((f) => f.id);
    if (!ids.length) {
      this.#notify('No out-of-scope requests to delete');
      return 0;
    }
    await this.deleteFlows(ids);
    if (resetIds) await api.resetFlowIds().catch(() => {});
    this.#notify(`Deleted ${ids.length} out-of-scope request${ids.length === 1 ? '' : 's'}${resetIds ? ' and reset flow ids' : ''}`);
    return ids.length;
  }

  async deleteFlows(ids) {
    if (!ids.length) return;
    await api.deleteFlows(ids);
    const gone = new Set(ids);
    // Move selection to the next remaining row so keyboard deletion can continue.
    const i = this.flows.findIndex((f) => f.id === this.selectedId);
    this.flows = this.flows.filter((f) => !gone.has(f.id));
    if (gone.has(this.selectedId)) this.selectedId = (this.flows[i] ?? this.flows[i - 1])?.id ?? null;
    this.selectedIds = new Set();
    // Drop passive findings for the removed flows so they can't become "ghost"
    // findings that point at a request no longer in History.
    if (this.passiveFindings.some((f) => gone.has(f.flowId))) {
      this.passiveFindings = this.passiveFindings.filter((f) => !gone.has(f.flowId));
    }
    for (const id of ids) this.#passiveScanned.delete(id);
    this.#invalidateDetails(ids);
    this.#savePassiveResults();
  }

  /** IDs an action should apply to: the multi-selection, else the single selected row. */
  get targetIds() {
    if (this.selectedIds.size) return [...this.selectedIds];
    return this.selectedId == null ? [] : [this.selectedId];
  }

  /** Cached flow detail, or null if it hasn't been loaded (synchronous peek). */
  peekDetail(id) {
    return id == null ? null : this.#detailCache.get(id) ?? null;
  }

  /**
   * Flow detail (get_flow), served from cache when possible so re-opening a
   * request is instant. Concurrent calls for the same id share one request, and
   * the result is cached (LRU, capped at DETAIL_CACHE_MAX).
   */
  async loadDetail(id) {
    if (id == null) return null;
    const hit = this.#detailCache.get(id);
    if (hit !== undefined) {
      // Refresh LRU position: delete + re-insert moves it to the newest slot.
      this.#detailCache.delete(id);
      this.#detailCache.set(id, hit);
      return hit;
    }
    const inflight = this.#detailInflight.get(id);
    if (inflight) return inflight;
    const p = api
      .getFlow(id)
      .then((d) => {
        this.#detailInflight.delete(id);
        if (d) this.#cacheDetail(id, d);
        return d;
      })
      .catch((e) => {
        this.#detailInflight.delete(id);
        throw e;
      });
    this.#detailInflight.set(id, p);
    return p;
  }

  /** Warm the cache for a row the user is likely to open next (hover, neighbour). */
  prefetchDetail(id) {
    if (id == null || this.#detailCache.has(id) || this.#detailInflight.has(id)) return;
    this.loadDetail(id).catch(() => {}); // best effort; a real open will surface errors
  }

  #cacheDetail(id, detail) {
    this.#detailCache.set(id, detail);
    while (this.#detailCache.size > this.detailCacheMax) {
      this.#detailCache.delete(this.#detailCache.keys().next().value);
    }
  }

  /** Change how many opened-flow details stay cached in memory (persisted per machine). */
  setDetailCacheMax(n) {
    const v = Math.max(5, Math.round(Number(n) || DETAIL_CACHE_MAX));
    this.detailCacheMax = v;
    try {
      localStorage.setItem(brandKey('detailCache'), String(v));
    } catch {
      /* not critical */
    }
    while (this.#detailCache.size > v) this.#detailCache.delete(this.#detailCache.keys().next().value);
  }

  // Drop cached details for flows that changed or went away, so the viewer never
  // shows stale bytes. Called on delete, on a streaming flow's final update, etc.
  #invalidateDetails(ids) {
    for (const id of ids) {
      this.#detailCache.delete(id);
      this.#detailInflight.delete(id);
    }
  }

  /** Row click. `rows` is the visible order, used for ⇧-click ranges. */
  selectRow(id, { toggle = false, range = false } = {}, rows = []) {
    if (range && this.selectedId != null) {
      const a = rows.findIndex((f) => f.id === this.selectedId);
      const b = rows.findIndex((f) => f.id === id);
      if (a !== -1 && b !== -1) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        this.selectedIds = new Set(rows.slice(lo, hi + 1).map((f) => f.id));
        return; // keep the anchor row's detail open
      }
    }
    if (toggle) {
      const next = new Set(this.selectedIds.size ? this.selectedIds : [this.selectedId].filter((x) => x != null));
      next.has(id) ? next.delete(id) : next.add(id);
      this.selectedIds = next;
      // Deselected the open row: show another selected one instead.
      if (!next.has(id)) {
        this.selectedId = [...next].at(-1) ?? null;
        return;
      }
    } else {
      this.selectedIds = new Set();
    }
    this.selectedId = id;
  }

  async setColorMany(ids, color) {
    await Promise.all(ids.map((id) => api.setFlowMeta(id, this.flows.find((f) => f.id === id)?.note ?? '', color)));
    const set = new Set(ids);
    this.flows = this.flows.map((f) => (set.has(f.id) ? { ...f, color } : f));
    this.#invalidateDetails(ids);
  }

  async setFlowMeta(id, { note, color }) {
    const flow = this.flows.find((f) => f.id === id);
    if (!flow) return;
    const next = { ...flow, note: note ?? flow.note, color: color ?? flow.color };
    await api.setFlowMeta(id, next.note, next.color);
    this.flows = this.flows.map((f) => (f.id === id ? next : f));
    this.#invalidateDetails([id]);
  }

  async exportHar() {
    const path = await save({
      title: 'Export HAR',
      defaultPath: `${this.project.name || BRAND.id}.har`,
      filters: [{ name: 'HTTP Archive', extensions: ['har'] }],
    });
    if (!path) return;
    try {
      const count = await api.exportHar(path);
      this.#notify(`Exported ${count} requests to HAR`);
    } catch (e) {
      this.#notify(String(e), true);
    }
  }

  async exportBurp() {
    const path = await save({
      title: 'Export Burp XML',
      defaultPath: `${this.project.name || BRAND.id}.xml`,
      filters: [{ name: 'Burp items', extensions: ['xml'] }],
    });
    if (!path) return;
    try {
      const count = await api.exportBurp(path);
      this.#notify(`Exported ${count} requests to Burp XML`);
    } catch (e) {
      this.#notify(String(e), true);
    }
  }

  // Save (compress) the project to its current .cegat document; falls back to
  // "Save as" when the project hasn't been saved to a file yet. Returns whether
  // the project was actually written (false if a Save-as dialog was cancelled).
  async saveProject() {
    // No document yet: prompt for a path (don't mask real save errors as "no doc").
    if (!this.project.path) return await this.saveProjectAs();
    return await this.#runSave(() => api.saveProject());
  }

  // Choose a path and save (compress) the project to it as a .cegat document.
  // Returns whether it was saved (false if cancelled or on error).
  async saveProjectAs() {
    const path = await save({
      title: 'Save project as',
      defaultPath: `${this.project.name || 'untitled'}.${BRAND.ext}`,
      filters: [{ name: `${BRAND.name} project`, extensions: [BRAND.ext] }],
    });
    if (!path) return false;
    return await this.#runSave(() => api.saveProjectAs(path));
  }

  // Shared save path: show the saving overlay, flush pending work, then save. The
  // backend does the snapshot + compression off the main thread, so the UI (and
  // this overlay's animation) stays responsive instead of freezing.
  async #runSave(fn) {
    this.saving = true;
    try {
      // The overlay paints while the (non-blocking, off-thread) save runs.
      await this.#flushRepeater();
      await this.#flushHistoryFilter();
      const r = await fn();
      this.project = { name: r.name, path: r.path };
      this.#notify(`Saved ${r.name} - ${r.summary}`);
      return true;
    } catch (e) {
      this.#notify(String(e), true);
      return false;
    } finally {
      this.saving = false;
      // Any save (manual or auto) resets the auto-save countdown, so it fires `interval`
      // after the latest save rather than on a fixed wall-clock cadence.
      this.#restartAutoSave();
    }
  }

  // Close the current project and return to the project chooser. The working copy
  // stays on disk (in the app data dir), so nothing is lost — Save writes it out
  // to a .ngintip document.
  async closeProject() {
    clearTimeout(this.#autoSaveTimer);
    this.#autoSaveTimer = null;
    await this.#flushRepeater();
    await this.#flushHistoryFilter();
    // Closing the project also closes any browser this app launched.
    await api.closeLaunchedBrowsers().catch(() => {});
    this.browser = { ...this.browser, opened: false };
    this.view = 'history';
    this.chooser = true;
  }

  #notify(text, error = false) {
    this.notice = { text, error };
    setTimeout(() => (this.notice = null), 3500);
  }

  /** Public transient toast (thin wrapper over the internal notifier). */
  notify(text, error = false) {
    this.#notify(text, error);
  }

  cycleTheme() {
    const order = ['system', 'light', 'dark'];
    this.theme = order[(order.indexOf(this.theme) + 1) % order.length];
    applyTheme(this.theme);
    try {
      localStorage.setItem(brandKey('theme'), this.theme);
    } catch {
      // not critical
    }
  }

  #onLog(entry) {
    // Bounded ring buffer; replaced (not mutated) so the table re-renders.
    const next = this.logs.length >= 2000 ? this.logs.slice(this.logs.length - 1999) : this.logs.slice();
    next.push(entry);
    this.logs = next;
    if (this.view !== 'logs' && (entry.level === 'error' || entry.level === 'warn')) this.logsUnread++;
  }

  async clearLogs() {
    await api.clearLogs();
    this.logs = [];
  }

  async deleteLogs(ids) {
    if (!ids.length) return;
    await api.deleteLogs(ids);
    const set = new Set(ids);
    this.logs = this.logs.filter((l) => !set.has(l.id));
  }

  seenLogs() {
    this.logsUnread = 0;
  }

  // Native File menu items (see build_menu in lib.rs) map to these actions.
  #onMenu(action) {
    ({
      new: () => this.newProject(),
      open: () => this.openProject(),
      save: () => this.saveProject(),
      save_as: () => this.saveProjectAs(),
      close_project: () => this.closeProject(),
      export_har: () => this.exportHar(),
      export_burp: () => this.exportBurp(),
    })[action]?.();
  }

  async flowBody(id, side) {
    return (await api.flowBody(id, side)) ?? '';
  }

  /** Image preview data URL, fetched on demand (Preview tab). */
  async flowImage(id) {
    return (await api.flowImage(id)) ?? null;
  }

  /** Hex dump of a flow body, fetched on demand (Pretty view for binary). */
  async flowHex(id, side) {
    return (await api.flowHex(id, side)) ?? '';
  }

  #resetPassive() {
    this.#passiveScanned = new Set();
    this.passiveFindings = [];
    this.passive = { scanning: false, done: 0, total: 0 };
  }

  clearPassive() {
    this.#resetPassive();
    this.passiveCollapsed = [];
    this.#savePassiveResults();
    this.#savePassiveCollapsed();
  }

  /** Remove findings by their unique `key`. The scanned-flow set is left intact,
   *  so an incremental scan won't resurrect what was deliberately deleted. */
  deletePassiveFindings(keys) {
    const set = keys instanceof Set ? keys : new Set(keys);
    if (!set.size) return;
    this.passiveFindings = this.passiveFindings.filter((f) => !set.has(f.key));
    this.#savePassiveResults();
  }

  /** Copy findings (defaults to all) to the clipboard as readable text. */
  copyPassiveFindings(findings) {
    const list = Array.isArray(findings) ? findings : this.passiveFindings;
    if (!list.length) return;
    const text = list
      .map((f) => `${(f.severity || '').toUpperCase().padEnd(6)} ${f.title}\t${f.match}\t${f.method || ''} ${f.host || ''}${f.path || ''} [${f.where}]`)
      .join('\n');
    this.copyText(text, `${list.length} finding${list.length === 1 ? '' : 's'}`);
  }

  async #loadPassiveConfig() {
    const rules = await api.getSetting(PASSIVE_RULES_SETTING).catch(() => null);
    const disabled = await api.getSetting(PASSIVE_DISABLED_SETTING).catch(() => null);
    const findings = await api.getSetting(PASSIVE_FINDINGS_SETTING).catch(() => null);
    const scanned = await api.getSetting(PASSIVE_SCANNED_SETTING).catch(() => null);
    const scope = await api.getSetting(PASSIVE_SCOPE_SETTING).catch(() => null);
    const inScopeOnly = await api.getSetting(PASSIVE_INSCOPE_SETTING).catch(() => null);
    const collapsed = await api.getSetting(PASSIVE_COLLAPSED_SETTING).catch(() => null);
    this.passiveRules = Array.isArray(rules) ? rules : [];
    this.passiveDisabled = Array.isArray(disabled) ? disabled : [];
    this.passiveScope = Array.isArray(scope) ? scope : [...DEFAULT_PASSIVE_SCOPE];
    this.passiveInScopeOnly = inScopeOnly === true;
    this.passiveCollapsed = Array.isArray(collapsed) ? collapsed : [];
    // Findings and which flows were scanned are historical: kept in the .cegat.
    this.passiveFindings = Array.isArray(findings) ? findings : [];
    this.#passiveScanned = new Set(Array.isArray(scanned) ? scanned : []);
    this.passive = { scanning: false, done: 0, total: 0 };
  }

  #savePassiveRules() {
    api.setSetting(PASSIVE_RULES_SETTING, this.passiveRules).catch((e) => this.#notify(`Failed to save rules: ${e}`, true));
  }

  #savePassiveDisabled() {
    api.setSetting(PASSIVE_DISABLED_SETTING, this.passiveDisabled).catch(() => {});
  }

  /** Set which resources passive scan looks at (list of extensions, or ['*']). */
  setPassiveScope(scope) {
    this.passiveScope = Array.isArray(scope) ? scope : [...DEFAULT_PASSIVE_SCOPE];
    api.setSetting(PASSIVE_SCOPE_SETTING, this.passiveScope).catch(() => {});
  }

  /** Toggle restricting the passive scan to hosts in the project Scope. */
  setPassiveInScopeOnly(on) {
    this.passiveInScopeOnly = !!on;
    api.setSetting(PASSIVE_INSCOPE_SETTING, this.passiveInScopeOnly).catch(() => {});
  }

  // Persist findings + scanned flow ids so results are historical (bounded).
  #savePassiveResults() {
    const findings = this.passiveFindings.slice(-10000);
    api.setSetting(PASSIVE_FINDINGS_SETTING, findings).catch(() => {});
    api.setSetting(PASSIVE_SCANNED_SETTING, [...this.#passiveScanned]).catch(() => {});
  }

  #savePassiveCollapsed() {
    api.setSetting(PASSIVE_COLLAPSED_SETTING, this.passiveCollapsed).catch(() => {});
  }

  /** Whether a finding group (by `${host}${path}` key) is collapsed. */
  passiveGroupCollapsed(key) {
    return this.passiveCollapsed.includes(key);
  }

  /** Expand/collapse one finding group; persisted per project. */
  togglePassiveGroup(key) {
    this.passiveCollapsed = this.passiveCollapsed.includes(key)
      ? this.passiveCollapsed.filter((k) => k !== key)
      : [...this.passiveCollapsed, key];
    this.#savePassiveCollapsed();
  }

  /** Replace the collapsed set (used by Expand all / Collapse all); persisted. */
  setPassiveCollapsed(keys) {
    this.passiveCollapsed = [...new Set(keys)];
    this.#savePassiveCollapsed();
  }

  /** Add a custom passive rule to this project. Returns '' or a validation error. */
  addPassiveRule({ title, severity = 'medium', pattern, flags = '' }) {
    const err = ruleError({ pattern, flags });
    if (err) return err;
    const rule = {
      id: `custom-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`,
      title: (title || '').trim() || 'Custom rule',
      severity,
      pattern,
      flags,
      custom: true,
      enabled: true,
    };
    this.passiveRules = [...this.passiveRules, rule];
    this.#savePassiveRules();
    return '';
  }

  removePassiveRule(id) {
    this.passiveRules = this.passiveRules.filter((r) => r.id !== id);
    this.#savePassiveRules();
  }

  toggleCustomRule(id) {
    this.passiveRules = this.passiveRules.map((r) => (r.id === id ? { ...r, enabled: r.enabled === false } : r));
    this.#savePassiveRules();
  }

  ruleEnabled(id) {
    return !this.passiveDisabled.includes(id);
  }

  toggleBuiltinRule(id) {
    this.passiveDisabled = this.passiveDisabled.includes(id)
      ? this.passiveDisabled.filter((r) => r !== id)
      : [...this.passiveDisabled, id];
    this.#savePassiveDisabled();
  }

  /** The rules that will actually run: enabled built-ins + enabled custom rules. */
  effectiveRules() {
    return [
      ...this.builtinRules.filter((r) => !this.passiveDisabled.includes(r.id)),
      ...this.passiveRules.filter((r) => r.enabled !== false),
    ];
  }

  /** How many high/medium findings there are (drives the sidebar badge). */
  get passiveAlerts() {
    return this.passiveFindings.filter((f) => f.severity === 'high' || f.severity === 'medium').length;
  }

  /**
   * Scan captured flows for secrets and risky headers. Only flows not scanned
   * yet are processed unless `rescan` is set. Bodies are fetched per flow, so
   * progress is reported as it goes.
   */
  async scanPassive({ rescan = false } = {}) {
    if (this.passive.scanning) return;
    if (rescan) this.#resetPassive();
    // Out-of-scope flows are left unscanned (not marked done), so widening the
    // scope later and scanning again picks them up. "In scope only" additionally
    // restricts to hosts in the project Scope (empty scope matches everything).
    const todo = this.flows.filter(
      (f) =>
        !this.#passiveScanned.has(f.id) &&
        flowInScope(f, this.passiveScope) &&
        (!this.passiveInScopeOnly || inScope(this.scope, f.host)),
    );
    if (!todo.length) {
      this.passive = { ...this.passive, scanning: false };
      return;
    }
    this.passive = { scanning: true, done: 0, total: todo.length };
    const compiled = compileRules(this.effectiveRules());
    const acc = [...this.passiveFindings];
    let done = 0;
    let cursor = 0;
    // Fetch bodies concurrently: getFlow is one IPC round-trip each, so doing them
    // one-at-a-time is the bottleneck. A small pool keeps the backend busy without
    // flooding it. The regex work between awaits is synchronous, so the shared
    // accumulator needs no locking.
    const scanOne = async (f) => {
      this.#passiveScanned.add(f.id);
      try {
        const d = await api.getFlow(f.id);
        if (!d) return;
        for (const item of scanFlow({ requestRaw: d.requestRaw, responseRaw: d.responseRaw, compiled })) {
          acc.push({
            ...item,
            flowId: f.id,
            method: f.method,
            host: f.host,
            path: f.path,
            status: f.status,
            key: `${f.id}|${item.type}|${item.where}|${item.match}`,
          });
        }
      } catch {
        // skip flows that fail to load
      }
      done++;
      if (done % 16 === 0) {
        this.passiveFindings = acc.slice();
        this.passive = { scanning: true, done, total: todo.length };
      }
    };
    const worker = async () => {
      while (cursor < todo.length) await scanOne(todo[cursor++]);
    };
    await Promise.all(Array.from({ length: Math.min(12, todo.length) }, worker));
    acc.sort((a, b) => SEVERITY_ORDER[a.severity] - SEVERITY_ORDER[b.severity] || a.flowId - b.flowId);
    this.passiveFindings = acc;
    this.passive = { scanning: false, done, total: todo.length };
    this.#savePassiveResults();
  }

  // A finding whose match History should highlight once the flow's detail loads.
  pendingFinding = $state(null);

  // Jump from a finding to its request in History. `finding` (optional) is the
  // finding to highlight (its match is searched in the request/response pane).
  showFinding(flowId, finding = null) {
    this.view = 'history';
    if (!this.flows.some((f) => f.id === flowId)) {
      this.#notify(`Request #${flowId} is no longer in History (it was deleted)`, true);
      return;
    }
    // Keep the user's History filters (scope, hide-types, Advanced, search) intact.
    // The request/response detail is driven by the selected id (from all flows, not
    // the filtered table), so it shows even when a filter would hide the row. If a
    // filter is hiding it, say so instead of wiping their filters.
    if (this.filtersActive && !this.#flowVisible(flowId)) {
      this.#notify(`Request #${flowId} is hidden by an active History filter — showing its detail below`);
    }
    // Highlight the match in the right pane once History has loaded the detail.
    this.pendingFinding = finding ? { flowId, where: finding.where, match: finding.match } : null;
    this.selectRow(flowId);
  }

  /** Whether a flow currently passes the History scope filter (a cheap proxy for
   *  "is it visible"; full advanced/text filtering lives in the view). */
  #flowVisible(flowId) {
    const f = this.flows.find((x) => x.id === flowId);
    if (!f) return false;
    if (this.scopeOnly && this.scope.length && !inScope(this.scope, f.host)) return false;
    return true;
  }

  // Put a highlighted snippet (e.g. from an HTTP/S response) into the full
  // Decoder and switch to it. The recipe is kept so the same pipeline can be
  // reused on the new input.
  sendStringToDecoder(text) {
    this.decoder.input = text ?? '';
    this.decoder.output = '';
    this.decoder.last = null;
    this.view = 'decoder';
  }

  // Comparer (Burp-style): a session list of captured messages, with two picked
  // for a side-by-side diff. Kept in memory so it survives switching tabs.
  comparer = $state({ items: [], a: null, b: null });
  #comparerSeq = 0;

  /** Add a message to the Comparer list; auto-fills the A then B slots. */
  addToComparer(text, label, kind = '') {
    const item = { id: `cmp-${++this.#comparerSeq}`, label: label || `Item ${this.comparer.items.length + 1}`, kind, text: text ?? '' };
    this.comparer.items = [...this.comparer.items, item];
    if (this.comparer.a == null) this.comparer.a = item.id;
    else if (this.comparer.b == null && this.comparer.b !== item.id) this.comparer.b = item.id;
    this.#notify(`Sent to Comparer (${this.comparer.items.length})`);
    return item.id;
  }

  /** Fetch a flow's request/response raw and add it to the Comparer. */
  async sendFlowToComparer(id, side) {
    const detail = await this.loadDetail(id);
    if (!detail) return;
    const f = this.flows.find((x) => x.id === id);
    const where = side === 'request' ? 'req' : 'res';
    const label = `#${id} ${where}${f ? ` ${f.method} ${f.host}${f.path}` : ''}`;
    this.addToComparer(side === 'request' ? detail.requestRaw : detail.responseRaw, label, where);
  }

  /** Add a blank item to type/paste into directly (returns its id). */
  newComparerItem() {
    const item = { id: `cmp-${++this.#comparerSeq}`, label: `Item ${this.comparer.items.length + 1}`, kind: 'text', text: '' };
    this.comparer.items = [...this.comparer.items, item];
    if (this.comparer.a == null) this.comparer.a = item.id;
    else if (this.comparer.b == null) this.comparer.b = item.id;
    return item.id;
  }

  removeComparerItem(itemId) {
    this.comparer.items = this.comparer.items.filter((it) => it.id !== itemId);
    if (this.comparer.a === itemId) this.comparer.a = null;
    if (this.comparer.b === itemId) this.comparer.b = null;
  }

  clearComparer() {
    this.comparer = { items: [], a: null, b: null };
  }

  async copyText(text, what) {
    try {
      await navigator.clipboard.writeText(text);
      this.#notify(`${what} copied`);
    } catch (e) {
      this.#notify(`Gagal menyalin: ${e}`, true);
    }
  }

  curlFor(detail) {
    const { scheme, host } = detail.summary;
    return toCurl(detail.requestRaw, `${scheme}://${host}`);
  }

  async copyCurl(id) {
    const detail = await api.getFlow(id);
    if (detail) await this.copyText(this.curlFor(detail), 'cURL command');
  }

  /** Copy a flow's request or response body (the full, uncapped body). */
  async copyBody(id, side) {
    const body = await this.flowBody(id, side);
    await this.copyText(body, side === 'request' ? 'Request body' : 'Response body');
  }

  async sendFlowToRepeater(id) {
    const detail = await api.getFlow(id);
    if (detail) this.sendToRepeater(detail);
  }

  async sendFlowToIntruder(id) {
    const detail = await api.getFlow(id);
    if (detail) this.sendToIntruder({ target: `${detail.summary.scheme}://${detail.summary.host}`, raw: detail.requestRaw });
  }

  sendToRepeater(detail) {
    const { scheme, host } = detail.summary;
    this.addRepeaterTab({ name: host, target: `${scheme}://${host}`, raw: detail.requestRaw });
  }

  addRepeaterTab(fields = {}) {
    const rep = this.repeater;
    const id = Math.max(0, ...rep.tabs.map((t) => t.id)) + 1;
    rep.tabs.push(makeTab(id, fields));
    rep.activeId = id;
    this.view = 'repeater';
  }

  closeRepeaterTab(id) {
    const rep = this.repeater;
    const i = rep.tabs.findIndex((t) => t.id === id);
    if (i === -1) return;
    // Tab ids get reused, so a closed tab's history must go with it.
    api.clearRepeaterHistory(id).catch(() => {});
    rep.tabs.splice(i, 1);
    if (!rep.tabs.length) rep.tabs.push(makeTab(1));
    if (rep.activeId === id) rep.activeId = (rep.tabs[i] ?? rep.tabs[i - 1]).id;
  }

  duplicateRepeaterTab(tab) {
    this.addRepeaterTab({ name: `${tab.name} copy`, group: tab.group, target: tab.target, raw: tab.raw });
  }

  // ---- Intruder ----
  addIntruderTab(fields = {}) {
    const it = this.intruder;
    const id = Math.max(0, ...it.tabs.map((t) => t.id)) + 1;
    it.tabs.push(makeIntruderTab(id, fields));
    it.activeId = id;
    this.view = 'intruder';
  }

  closeIntruderTab(id) {
    const it = this.intruder;
    const i = it.tabs.findIndex((t) => t.id === id);
    if (i === -1) return;
    it.tabs[i].running = false;
    it.tabs.splice(i, 1);
    if (!it.tabs.length) it.tabs.push(makeIntruderTab(1));
    if (it.activeId === id) it.activeId = (it.tabs[i] ?? it.tabs[i - 1]).id;
  }

  sendToIntruder({ target, raw }) {
    this.addIntruderTab({ target, raw });
  }

  stopAttack(tab) {
    tab.running = false;
  }

  // ---- Python extensions ----
  async #loadExtensions() {
    try {
      this.#applyExtStatus(await api.extensionsStatus());
    } catch {
      /* leave defaults */
    }
  }
  #applyExtStatus(s) {
    if (!s) return;
    this.extensions = {
      ...this.extensions,
      enabled: !!s.enabled,
      python: s.python ?? '',
      venv: s.venv ?? '',
      venvReady: !!s.venvReady,
      list: s.extensions ?? [],
    };
  }
  #extSpecs() {
    return this.extensions.list.map((e) => ({
      id: e.id,
      name: e.name,
      path: e.path,
      enabled: e.enabled,
      scope: e.scope ?? 'all',
      requirements: e.requirements ?? [],
    }));
  }
  async #pushExtensions(specs) {
    this.#applyExtStatus(await api.setExtensions(specs));
  }
  async addExtension({ name, path }) {
    if (!path) return;
    const id = `ext-${Date.now().toString(36)}`;
    const spec = { id, name: name || path.split(/[\\/]/).pop(), path, enabled: true, scope: 'all', requirements: [] };
    await this.#pushExtensions([...this.#extSpecs(), spec]);
  }
  async removeExtension(id) {
    await this.#pushExtensions(this.#extSpecs().filter((e) => e.id !== id));
  }
  async toggleExtension(id) {
    await this.#pushExtensions(this.#extSpecs().map((e) => (e.id === id ? { ...e, enabled: !e.enabled } : e)));
  }
  async setExtensionScope(id, scope) {
    await this.#pushExtensions(this.#extSpecs().map((e) => (e.id === id ? { ...e, scope } : e)));
  }
  /** Whether a scope string covers a given surface ("proxy"|"interceptor"|"repeater"). */
  extensionSurfaceOn(scope, surface) {
    return extScopeSet(scope).has(surface);
  }
  /** Toggle one surface in an extension's scope set (keeps at least one surface on). */
  async toggleExtensionSurface(id, surface) {
    await this.#pushExtensions(
      this.#extSpecs().map((e) => {
        if (e.id !== id) return e;
        const set = extScopeSet(e.scope);
        if (set.has(surface)) {
          if (set.size <= 1) return e; // never leave an extension with no surface
          set.delete(surface);
        } else {
          set.add(surface);
        }
        return { ...e, scope: extScopeString(set) };
      }),
    );
  }
  async setExtensionsEnabled(on) {
    this.#applyExtStatus(await api.setExtensionsEnabled(on));
  }
  async setPythonPath(path) {
    this.#applyExtStatus(await api.setPythonPath(path ?? ''));
  }
  async detectExtensionPython() {
    const p = await api.detectPython();
    if (p) await this.setPythonPath(p);
    else this.#notify('No python3 / python found on PATH. Set the path manually.', true);
  }
  async setVenvPath(path) {
    this.#applyExtStatus(await api.setExtensionVenvPath(path ?? ''));
  }
  async testExtension(id, phase, raw) {
    return await api.testExtension(id, phase, raw);
  }
  async extensionTemplate() {
    return await api.extensionTemplate();
  }
  async listExtensionPackages() {
    try {
      return await api.extensionPackages();
    } catch {
      return [];
    }
  }
  async installExtensionPackage(packages) {
    this.extensions = { ...this.extensions, busy: true };
    try {
      const log = await api.installExtensionPackage(packages);
      this.extensions = { ...this.extensions, log };
      this.#notify('Package installed');
      await this.#loadExtensions();
      return true;
    } catch (e) {
      this.extensions = { ...this.extensions, log: String(e) };
      this.#notify(String(e), true);
      return false;
    } finally {
      this.extensions = { ...this.extensions, busy: false };
    }
  }
  async setupExtensionVenv() {
    this.extensions = { ...this.extensions, busy: true };
    try {
      const log = await api.setupExtensionVenv();
      this.extensions = { ...this.extensions, log };
      this.#notify('Extension venv is ready');
      await this.#loadExtensions();
    } catch (e) {
      this.extensions = { ...this.extensions, log: String(e) };
      this.#notify(String(e), true);
    } finally {
      this.extensions = { ...this.extensions, busy: false };
    }
  }

  async startAttack(tab) {
    if (tab.running) return;
    const gen = generate(tab.raw, tab.type, tab.sets);
    if (gen.error) {
      tab.error = gen.error;
      return;
    }
    tab.error = gen.truncated ? `Capped at ${MAX_REQUESTS} requests` : '';
    const { requests } = gen;
    tab.total = requests.length;
    tab.sent = 0;
    tab.selected = null;
    tab.results = [];
    tab.running = true;

    const results = new Array(requests.length);
    let cursor = 0;
    let filled = 0;
    const flush = () => (tab.results = results.slice(0, filled).filter(Boolean));

    const worker = async () => {
      while (tab.running) {
        const i = cursor++;
        if (i >= requests.length) break;
        const req = requests[i];
        const started = performance.now();
        let row;
        try {
          const res = await api.sendRaw(tab.target, req.request);
          row = {
            n: i + 1,
            payloads: req.payloads,
            status: res.status,
            length: res.length,
            timeMs: res.durationMs ?? Math.round(performance.now() - started),
            error: null,
            requestRaw: i < KEEP_RESPONSES ? req.request.slice(0, RESPONSE_CAP) : null,
            responseRaw: i < KEEP_RESPONSES ? (res.responseRaw ?? '').slice(0, RESPONSE_CAP) : null,
          };
        } catch (e) {
          row = {
            n: i + 1,
            payloads: req.payloads,
            status: 0,
            length: 0,
            timeMs: Math.round(performance.now() - started),
            error: String(e),
            requestRaw: i < KEEP_RESPONSES ? req.request.slice(0, RESPONSE_CAP) : null,
            responseRaw: null,
          };
        }
        results[i] = row;
        filled = Math.max(filled, i + 1);
        tab.sent++;
        // Throttle reactive updates.
        if (tab.sent % 25 === 0) flush();
      }
    };

    const pool = Math.max(1, Math.min(tab.concurrency || 1, requests.length));
    await Promise.all(Array.from({ length: pool }, worker));
    flush();
    tab.running = false;
  }


  // Per-tab so switching tabs mid-flight doesn't mix up responses.
  async sendRepeater(tab) {
    if (tab.sending) return;
    tab.sending = true;
    tab.error = null;
    await this.loadRepeaterHistory(tab);
    const lastId = tab.history.at(-1)?.id;
    try {
      tab.response = await api.sendRaw(tab.target, tab.raw, tab.id);
    } catch (e) {
      tab.response = null;
      // A user-initiated cancel isn't an error; show a quiet note instead.
      tab.error = /cancel/i.test(String(e)) ? null : String(e);
    } finally {
      tab.sending = false;
    }
    // The send is recorded in history, but stay in live editing mode so the
    // request stays directly editable (use < to review past sends).
    tab.history = await api.repeaterHistory(tab.id).catch(() => tab.history);
    if (tab.history.at(-1)?.id !== lastId) {
      tab.pos = null;
      tab.draft = null;
    }
  }

  /** Cancel a slow in-flight send for this Repeater tab. */
  async cancelRepeater(tab) {
    if (!tab?.sending) return;
    await api.cancelSend(tab.id).catch(() => {});
  }

  /**
   * Send every tab in a Repeater group. `mode`:
   *  - 'sequential': one after another (each waits for the previous)
   *  - 'parallel': all at once (concurrent, like multi-threaded)
   */
  async sendGroup(name, mode = 'sequential') {
    const g = name || '';
    const tabs = this.repeater.tabs.filter((t) => (t.group || '') === g && !t.sending);
    if (!tabs.length) return;
    if (mode === 'parallel') {
      await Promise.all(tabs.map((t) => this.sendRepeater(t)));
    } else {
      for (const t of tabs) await this.sendRepeater(t);
    }
    this.#notify(`Sent ${tabs.length} request${tabs.length === 1 ? '' : 's'} (${mode})`);
  }

  /** Open the redirect target of a held Intercept response as a new Repeater tab. */
  followHeldRedirect(item) {
    if (!item) return;
    const reqRaw = `${item.method} ${item.path} HTTP/1.1\nHost: ${item.host}\n\n`;
    const next = followRequest(reqRaw, item.raw ?? '', `${item.scheme}://${item.host}`);
    if (!next) {
      this.#notify('No redirect (3xx + Location) to follow', true);
      return;
    }
    this.addRepeaterTab({ name: `↳ ${next.status}`, target: next.target, raw: next.raw });
    this.view = 'repeater';
    this.#notify('Redirect target opened in Repeater');
  }

  /** Follow the redirect in a Repeater tab's last response and send it. */
  async followRedirect(tab) {
    const next = followRequest(tab.raw, tab.response?.responseRaw ?? '', tab.target);
    if (!next) {
      this.#notify('No redirect (3xx + Location) to follow', true);
      return;
    }
    tab.target = next.target;
    tab.raw = next.raw;
    tab.pos = null;
    tab.draft = null;
    await this.sendRepeater(tab);
  }

  async loadRepeaterHistory(tab) {
    if (tab.historyLoaded) return;
    tab.historyLoaded = true;
    tab.history = await api.repeaterHistory(tab.id).catch(() => []);
    // Show the latest response straight away (a reopened/saved project doesn't
    // persist the response, so without this the pane is blank until you click <).
    // Only when nothing is already shown this session (don't clobber a live send).
    if (tab.history.length && !tab.response && !tab.error && tab.pos === null) {
      const entry = tab.history[tab.history.length - 1];
      if (entry.error) {
        tab.error = entry.error;
        return;
      }
      tab.loadingEntry = true;
      try {
        const full = await api.repeaterEntry(entry.id);
        // Bail if the user sent/navigated meanwhile.
        if (full && tab.pos === null && !tab.response) {
          tab.response = {
            status: full.status,
            durationMs: full.durationMs,
            length: full.length,
            responseRaw: full.responseRaw,
            responseImage: null,
          };
        }
      } finally {
        tab.loadingEntry = false;
      }
    }
  }

  /** Show the previous (`step` -1) or next (+1) request sent from this tab. */
  async repeaterGo(tab, step) {
    const h = tab.history;
    const next = tab.pos === null ? (step < 0 ? h.length - 1 : h.length) : tab.pos + step;
    if (next < 0 || next > h.length) return;

    if (next === h.length) {
      // Past the newest entry: back to the edits set aside earlier.
      if (!tab.draft) return;
      Object.assign(tab, tab.draft, { draft: null, pos: null });
      return;
    }

    // Keep unsent edits reachable at the end of the history.
    const shown = h[tab.pos ?? h.length - 1];
    if (!shown || shown.raw !== tab.raw || shown.target !== tab.target) {
      tab.draft = { raw: tab.raw, target: tab.target, response: tab.response, error: tab.error };
    }

    const entry = h[next];
    tab.pos = next;
    tab.raw = entry.raw;
    tab.target = entry.target;
    tab.error = entry.error;
    tab.response = null;
    if (entry.error) return;
    tab.loadingEntry = true;
    try {
      const full = await api.repeaterEntry(entry.id);
      // Ignore if the user moved on meanwhile.
      if (full && tab.pos === next) {
        tab.response = {
          status: full.status,
          durationMs: full.durationMs,
          length: full.length,
          responseRaw: full.responseRaw,
          responseImage: null,
        };
      }
    } finally {
      tab.loadingEntry = false;
    }
  }

  /** Set a Repeater note. scope: 'tab' (the active tab), 'group' (every tab in the
   *  active tab's group), or 'all' (every tab). Notes persist with the project. */
  setRepeaterNote(text, scope = 'tab') {
    const active = this.repeater.tabs.find((t) => t.id === this.repeater.activeId);
    if (!active) return;
    const note = text ?? '';
    if (scope === 'all') {
      for (const t of this.repeater.tabs) t.note = note;
    } else if (scope === 'group') {
      const g = active.group || '';
      for (const t of this.repeater.tabs) if ((t.group || '') === g) t.note = note;
    } else {
      active.note = note;
    }
    this.saveRepeater();
  }

  /** How many tabs a note scope would write to (for the popup label). */
  repeaterNoteScopeCount(scope) {
    const active = this.repeater.tabs.find((t) => t.id === this.repeater.activeId);
    if (!active) return 0;
    if (scope === 'all') return this.repeater.tabs.length;
    if (scope === 'group') return this.repeater.tabs.filter((t) => (t.group || '') === (active.group || '')).length;
    return 1;
  }

  /** Save tabs to the project, debounced. Reads state synchronously so an $effect tracks it. */
  saveRepeater() {
    const json = JSON.stringify(repeaterSnapshot(this.repeater));
    if (json === this.#repeaterSaved) return;
    clearTimeout(this.#repeaterTimer);
    this.#repeaterTimer = setTimeout(() => this.#flushRepeater(), 400);
  }

  async #flushRepeater() {
    clearTimeout(this.#repeaterTimer);
    this.#repeaterTimer = null;
    const snapshot = repeaterSnapshot(this.repeater);
    const json = JSON.stringify(snapshot);
    if (json === this.#repeaterSaved) return;
    try {
      await api.setSetting(REPEATER_SETTING, snapshot);
      this.#repeaterSaved = json;
    } catch (e) {
      this.#notify(`Failed to save Repeater tabs: ${e}`, true);
    }
  }

  async #loadRepeater() {
    let saved = await api.getSetting(REPEATER_SETTING).catch(() => null);
    const legacy = !saved?.tabs?.length && takeLegacyRepeater();
    if (legacy?.tabs?.length) saved = legacy;
    const tabs = saved?.tabs?.length ? saved.tabs.map((t) => makeTab(t.id, t)) : [makeTab(1)];
    // Mutate in place: views hold a reference to this object.
    this.repeater.tabs = tabs;
    this.repeater.groupColors = saved?.groupColors ?? {};
    this.repeater.collapsedGroups = Array.isArray(saved?.collapsedGroups) ? saved.collapsedGroups : [];
    this.repeater.activeId = tabs.some((t) => t.id === saved?.activeId) ? saved.activeId : tabs[0].id;
    this.#repeaterSaved = legacy ? '' : JSON.stringify(repeaterSnapshot(this.repeater));
    if (legacy) await this.#flushRepeater();
  }

  // Load all per-project view settings (columns, hide filter, passive rules).
  async #loadProjectSettings() {
    await this.#loadHistoryColumns();
    await this.#loadHistorySort();
    await this.#loadHistoryHide();
    await this.#loadHistoryFilter();
    await this.#loadAutoSave();
    await this.#loadPassiveConfig();
    await this.#loadInterceptFilter();
    await this.#loadCdp();
    await this.#loadProfiles();
    // Extensions are per-project: the backend loads the open project's set, so
    // refresh the UI list here too (runs on init, blankProject and switchProject).
    await this.#loadExtensions();
  }

  async #loadCdp() {
    const saved = await api.getSetting(BROWSER_CDP_SETTING).catch(() => null);
    const port = Number.isInteger(saved?.port) && saved.port > 0 && saved.port < 65536 ? saved.port : 9222;
    this.cdp = { enabled: saved?.enabled === true, port };
  }

  /** Enable/disable CDP and/or set its port for the launched browser (per project). */
  setCdp({ enabled, port } = {}) {
    const next = {
      enabled: enabled === undefined ? this.cdp.enabled : !!enabled,
      port: port === undefined ? this.cdp.port : Math.min(65535, Math.max(1, Math.trunc(port) || 9222)),
    };
    this.cdp = next;
    api.setSetting(BROWSER_CDP_SETTING, next).catch(() => {});
  }

  // ---- Browser profiles (per project) ----
  async #loadProfiles() {
    const s = await api.getSetting(BROWSER_PROFILES_SETTING).catch(() => null);
    this.profiles = {
      persist: s?.persist === true,
      multi: s?.multi === true,
      list: Array.isArray(s?.list) ? s.list.filter((n) => typeof n === 'string') : [],
    };
  }
  setProfiles(patch) {
    this.profiles = { ...this.profiles, ...patch };
    api.setSetting(BROWSER_PROFILES_SETTING, this.profiles).catch(() => {});
  }
  addProfile() {
    const n = this.profiles.list.length + 1;
    this.setProfiles({ list: [...this.profiles.list, `profile ${n}`] });
  }
  renameProfile(i, name) { this.setProfiles({ list: this.profiles.list.map((p, j) => (j === i ? name : p)) }); }
  removeProfile(i) { this.setProfiles({ list: this.profiles.list.filter((_, j) => j !== i) }); }
  /** Launch a browser on a specific named profile (from the Browser settings). */
  spawnBrowserProfile(name) { return this.openPentestBrowser(undefined, name || 'default'); }

  // ---- Auto-save (per project) ----
  async #loadAutoSave() {
    const saved = await api.getSetting(AUTOSAVE_SETTING).catch(() => null);
    if (saved && typeof saved === 'object') {
      // The project has its own config: respect it exactly.
      const interval = Number(saved.interval);
      this.autoSave = {
        enabled: saved.enabled === true,
        interval: Number.isFinite(interval) && interval >= 5 ? Math.round(interval) : 3600,
      };
    } else {
      // No config in this project yet: default to on, every hour.
      this.autoSave = { enabled: true, interval: 3600 };
    }
    this.#restartAutoSave();
  }

  /** Enable/disable auto-save or change its interval; persisted with the project. */
  setAutoSave({ enabled, interval } = {}) {
    const next = {
      enabled: enabled ?? this.autoSave.enabled,
      interval: Math.max(5, Math.round(Number(interval ?? this.autoSave.interval) || 3600)),
    };
    this.autoSave = next;
    api.setSetting(AUTOSAVE_SETTING, next).catch(() => {});
    this.#restartAutoSave();
  }

  #restartAutoSave() {
    clearTimeout(this.#autoSaveTimer);
    this.#autoSaveTimer = null;
    if (!this.autoSave.enabled) return;
    // One-shot timer, re-armed after every save (see #runSave), so the next auto-save is
    // always measured from the LAST save - a manual save at minute 55 pushes the next
    // auto-save to minute 115, instead of a redundant one firing 5 minutes later.
    // Re-arming a timer is O(1), so there is no performance cost to this.
    this.#autoSaveTimer = setTimeout(() => this.#autoSaveTick(), this.autoSave.interval * 1000);
  }

  async #autoSaveTick() {
    // The project was closed; a fresh open re-arms auto-save, so just stop here.
    if (this.chooser) return;
    // Can't save right now (no file yet, or a save is already running): don't pop a
    // Save-as dialog on a tick - just check again after another interval.
    if (!this.project.path || this.saving) {
      this.#restartAutoSave();
      return;
    }
    // #runSave re-arms the timer in its finally, so the next auto-save is interval-after.
    await this.#runSave(() => api.saveProject());
  }

  // ---- History scope + advanced filter (persisted per project) ----
  async #loadHistoryFilter() {
    const saved = await api.getSetting(HISTORY_FILTER_SETTING).catch(() => null);
    const base = emptyAdvanced();
    const a = saved && typeof saved.adv === 'object' && saved.adv ? saved.adv : {};
    const oneOf = (v, allowed, fallback) => (allowed.includes(v) ? v : fallback);
    this.adv = {
      methods: Array.isArray(a.methods) ? a.methods : base.methods,
      status: Array.isArray(a.status) ? a.status : base.status,
      reqBody: oneOf(a.reqBody, ['any', 'yes', 'no'], base.reqBody),
      resBody: oneOf(a.resBody, ['any', 'yes', 'no'], base.resBody),
      minMs: a.minMs ?? base.minMs,
      maxMs: a.maxMs ?? base.maxMs,
      filters: Array.isArray(a.filters) ? a.filters.map((r) => newFilter(r)) : base.filters,
    };
    this.scopeOnly = saved?.scopeOnly === true;
    // Record what we loaded so the first autosave pass doesn't rewrite it.
    this.#historyFilterSaved = JSON.stringify({ scopeOnly: this.scopeOnly, adv: this.adv });
  }

  /** Save the scope + advanced filter to the project, debounced. Reads state
   *  synchronously so an $effect in HistoryView tracks every field. */
  saveHistoryFilter() {
    const json = JSON.stringify({ scopeOnly: this.scopeOnly, adv: this.adv });
    if (json === this.#historyFilterSaved) return;
    clearTimeout(this.#historyFilterTimer);
    this.#historyFilterTimer = setTimeout(() => this.#flushHistoryFilter(), 400);
  }

  async #flushHistoryFilter() {
    clearTimeout(this.#historyFilterTimer);
    this.#historyFilterTimer = null;
    const snapshot = { scopeOnly: this.scopeOnly, adv: this.adv };
    const json = JSON.stringify(snapshot);
    if (json === this.#historyFilterSaved) return;
    try {
      await api.setSetting(HISTORY_FILTER_SETTING, snapshot);
      this.#historyFilterSaved = json;
    } catch (e) {
      this.#notify(`Failed to save history filter: ${e}`, true);
    }
  }

  // ---- Intercept advanced filter (persisted per project) ----
  async #loadInterceptFilter() {
    let rules = [];
    try {
      const saved = await api.getSetting(INTERCEPT_FILTER_SETTING);
      if (saved && Array.isArray(saved.rules)) rules = saved.rules.map(normalizeInterceptRule);
    } catch {
      rules = [];
    }
    this.intercept.filter = { rules };
    // Push the loaded filter to the proxy so it takes effect immediately.
    api.setInterceptFilter(rules).catch(() => {});
  }

  /** Persist the intercept filter to the project and push it to the proxy. */
  applyInterceptFilter() {
    const rules = (this.intercept.filter.rules ?? []).map(normalizeInterceptRule);
    api.setInterceptFilter(rules).catch((e) => this.#notify(String(e), true));
    api.setSetting(INTERCEPT_FILTER_SETTING, { rules }).catch(() => {});
  }

  addInterceptFilterRule(patch = {}) {
    this.intercept.filter = { rules: [...(this.intercept.filter.rules ?? []), newInterceptRule(patch)] };
  }

  removeInterceptFilterRule(i) {
    const rules = (this.intercept.filter.rules ?? []).slice();
    rules.splice(i, 1);
    this.intercept.filter = { rules };
    this.applyInterceptFilter();
  }

  clearInterceptFilter() {
    this.intercept.filter = { rules: [] };
    this.applyInterceptFilter();
  }

  async #loadHistoryColumns() {
    const saved = await api.getSetting(HISTORY_COLUMNS_SETTING).catch(() => null);
    this.historyColumns = Array.isArray(saved) && saved.length ? normalizeColumns(saved) : [...DEFAULT_COLUMN_KEYS];
  }

  async #loadHistorySort() {
    const s = await api.getSetting(HISTORY_SORT_SETTING).catch(() => null);
    this.historySort = s && typeof s.key === 'string' && (s.dir === 1 || s.dir === -1) ? { key: s.key, dir: s.dir } : { key: 'id', dir: 1 };
  }

  /** Sort the History table by a column; clicking the active column flips direction.
   *  Persisted per project so re-opening keeps the order. */
  setHistorySort(key) {
    const cur = this.historySort;
    this.historySort = cur.key === key ? { key, dir: -cur.dir } : { key, dir: 1 };
    api.setSetting(HISTORY_SORT_SETTING, this.historySort).catch(() => {});
  }

  async #loadHistoryHide() {
    const saved = await api.getSetting(HISTORY_HIDE_SETTING).catch(() => null);
    this.historyHide = saved && Array.isArray(saved.cats) ? { cats: saved.cats, ext: saved.ext ?? '', exclude: saved.exclude !== false } : emptyHide();
  }

  async #saveHistoryHide() {
    try {
      await api.setSetting(HISTORY_HIDE_SETTING, this.historyHide);
    } catch (e) {
      this.#notify(`Failed to save hide filter: ${e}`, true);
    }
  }

  toggleHideCategory(key) {
    const cats = this.historyHide.cats;
    this.historyHide = { ...this.historyHide, cats: cats.includes(key) ? cats.filter((c) => c !== key) : [...cats, key] };
    this.#saveHistoryHide();
  }

  setHideExt(ext) {
    this.historyHide = { ...this.historyHide, ext };
    this.#saveHistoryHide();
  }

  setHideExclude(exclude) {
    this.historyHide = { ...this.historyHide, exclude };
    this.#saveHistoryHide();
  }

  clearHide() {
    this.historyHide = emptyHide();
    this.#saveHistoryHide();
  }

  // Replace the visible History columns (persisted for this project).
  async setHistoryColumns(keys) {
    this.historyColumns = normalizeColumns(keys);
    try {
      await api.setSetting(HISTORY_COLUMNS_SETTING, this.historyColumns);
    } catch (e) {
      this.#notify(`Failed to save columns: ${e}`, true);
    }
  }

  toggleColumn(key) {
    const set = new Set(this.historyColumns);
    set.has(key) ? set.delete(key) : set.add(key);
    this.setHistoryColumns([...set]);
  }

  resetAdvanced() {
    this.adv = emptyAdvanced();
  }

  /** Any History filter that could hide rows is active. */
  get filtersActive() {
    return !!(this.filter.trim() || this.scopeOnly || this.bodyHits || this.sitemapNode || advActive(this.adv));
  }

  /** Reset every History filter so all captured requests show again. */
  clearAllFilters() {
    this.filter = '';
    this.scopeOnly = false;
    this.bodyHits = null;
    this.sitemapNode = null;
    this.resetAdvanced();
    this.clearHide();
  }

  // Toggle a value in one of the advanced-filter array fields (methods/status).
  toggleAdv(field, value) {
    const list = this.adv[field];
    this.adv[field] = list.includes(value) ? list.filter((v) => v !== value) : [...list, value];
  }

  // ---- Filters modal (multiple include/exclude rules) -----------------------

  addAdvFilter(patch = {}) {
    this.adv.filters = [...(this.adv.filters ?? []), newFilter(patch)];
  }
  removeAdvFilter(i) {
    this.adv.filters = (this.adv.filters ?? []).filter((_, n) => n !== i);
  }
  clearAdvFilters() {
    this.adv.filters = [];
  }
  /**
   * Append exclude rules for the common static-asset junk (js/css/images/fonts/
   * maps). Skips any rule already present, so clicking twice adds nothing new.
   * Returns how many rules were added (for the toast).
   */
  fillBasicExcludes() {
    const existing = this.adv.filters ?? [];
    const has = (field, value) =>
      existing.some(
        (r) => r.field === field && r.mode === 'exclude' && String(r.value).trim().toLowerCase() === value.toLowerCase(),
      );
    const additions = BASIC_EXCLUDE_FILTERS.filter((p) => !has(p.field, p.value)).map((p) =>
      newFilter({ field: p.field, mode: 'exclude', value: p.value }),
    );
    if (additions.length) this.adv.filters = [...existing, ...additions];
    this.notify(additions.length ? `Added ${additions.length} static-asset exclude rules` : 'Static-asset excludes already present');
    return additions.length;
  }
  /** Mute or unmute one rule without deleting it (keeps the row for later). */
  toggleAdvFilter(i) {
    this.adv.filters = (this.adv.filters ?? []).map((r, n) => (n === i ? { ...r, enabled: r.enabled === false } : r));
  }
  /** Turn every rule on or off at once (so "research all" needs no delete/re-add). */
  setAllAdvFilters(enabled) {
    this.adv.filters = (this.adv.filters ?? []).map((r) => ({ ...r, enabled: !!enabled }));
  }
  /** True when there is at least one rule and all of them are enabled. */
  get advFiltersAllOn() {
    const rules = this.adv.filters ?? [];
    return rules.length > 0 && rules.every((r) => r.enabled !== false);
  }
  /** Number of enabled rules with a value (drives the "Filters (N)" badge). */
  get advFilterCount() {
    return (this.adv.filters ?? []).filter((r) => r && r.enabled !== false && String(r.value).trim() !== '').length;
  }

}

export const store = new Store();
