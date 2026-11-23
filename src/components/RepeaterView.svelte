<script>
  import CodeEditor from './CodeEditor.svelte';
  import MessageView from './MessageView.svelte';
  import { repeaterPref } from '../lib/viewprefs.svelte.js';
  import MiniDecoder from './MiniDecoder.svelte';
  import SplitPane from './SplitPane.svelte';
  import CaptureEditor from './CaptureEditor.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import WsView from './WsView.svelte';
  import Modal from './Modal.svelte';
  import { store } from '../lib/store.svelte.js';
  import { fmtSize, fmtDateTime, statusClass } from '../lib/format.js';
  import { brandKey } from '../lib/brand.js';
  import { prettyBody, splitMessage, prettyJsonText } from '../lib/message.js';
  import { isHandshakeRequest, isSocketUrl } from '../lib/wsframe.js';
  import { headerMenuItems, bodyMenuItems } from '../lib/editormenu.js';
  import { setHeader } from '../lib/httpmsg.js';
  import { redirectInfo } from '../lib/redirect.js';
  import { toCurl } from '../lib/curl.js';
  import { HIGHLIGHTS } from '../lib/colors.js';

  const rep = store.repeater;
  let tab = $derived(rep.tabs.find((t) => t.id === rep.activeId) ?? rep.tabs[0]);
  let renaming = $state(null);
  let capture = $state(null);
  let menu = $state(null);
  // The tab bar: a vertical mouse wheel scrolls it sideways when there are more
  // tabs than fit (otherwise overflowing tabs are stuck off to the right).
  let tabsEl = $state();
  function onTabsWheel(e) {
    if (wrapTabs || !tabsEl || tabsEl.scrollWidth <= tabsEl.clientWidth) return;
    if (Math.abs(e.deltaY) <= Math.abs(e.deltaX)) return; // let native horizontal wheels through
    tabsEl.scrollLeft += e.deltaY;
    e.preventDefault();
  }
  // Tab layout: single scrollable row (default) or wrapped onto multiple rows that
  // stack downward. Remembered per machine (a view preference, like split ratios).
  const WRAP_KEY = brandKey('repeater.wrapTabs');
  let wrapTabs = $state(loadWrap());
  function loadWrap() {
    try {
      return localStorage.getItem(WRAP_KEY) === '1';
    } catch {
      return false;
    }
  }
  function toggleWrap() {
    wrapTabs = !wrapTabs;
    try {
      localStorage.setItem(WRAP_KEY, wrapTabs ? '1' : '0');
    } catch {
      // storage unavailable; still applies for this session
    }
  }
  // Highlight-to-decode side bar (opens when text is selected in req/response).
  let miniDecode = $state(null);
  // Reset the decoder dock when switching tabs so it doesn't show a stale snippet.
  $effect(() => {
    rep.activeId;
    miniDecode = null;
  });
  let notes = $state(null); // { scope, text } | null
  const openNotes = () => (notes = { scope: 'tab', text: tab.note || '' });
  function saveNotes() {
    store.setRepeaterNote(notes.text, notes.scope);
    notes = null;
  }
  let groupModal = $state(null); // { mode:'new'|'rename', tabId?, oldName?, name }
  // Collapsed group names live in the (persisted) repeater state, so a collapsed
  // group stays collapsed after switching views or reopening the project.
  const isCollapsed = (name) => rep.collapsedGroups?.includes(name);

  // When a repeater-scope extension transforms the request/response, the send returns
  // both versions so the user can compare what was sent/received vs typed/arrived.
  let reqView = $state('typed'); // 'typed' (editable) | 'sent' (read-only, extension output)
  let resView = $state('final'); // 'final' (transformed) | 'original' (as it arrived)
  let reqFinal = $derived(tab?.response?.requestFinal ?? null);
  let resOriginal = $derived(tab?.response?.responseOriginal ?? null);
  function pickReqView(id) {
    reqView = id;
    repeaterPref.req.pick(id);
  }
  function pickResView(id) {
    resView = id;
    repeaterPref.res.pick(id);
  }
  // Reopen on the version last chosen when the active tab changes, but only when this
  // send actually produced it: without a transformed request there is no Sent to show,
  // and without an arrived original there is no Original.
  $effect(() => {
    tab?.id;
    reqView = repeaterPref.req.openOn(reqFinal ? ['typed', 'sent'] : ['typed']);
    resView = repeaterPref.res.openOn(resOriginal ? ['final', 'original'] : ['final']);
  });

  // In-pane find/search. The bottom-bar Find button opens the search on whichever
  // pane the user last interacted with (Cmd/Ctrl+F targets the hovered pane directly).
  let reqRef = $state();
  let resRef = $state();
  let activePane = $state('req');
  function openFind() {
    const ref = activePane === 'req' ? reqRef : resRef;
    ref?.openFind?.();
  }

  // The absolute URL of the current request (request-line target resolved against
  // the tab's target host), for Copy URL / cURL.
  function requestUrl() {
    const line = (tab.raw || '').split('\n')[0] || '';
    const path = line.split(/\s+/)[1] || '/';
    if (/^https?:\/\//i.test(path)) return path;
    return `${(tab.target || '').replace(/\/+$/, '')}${path}`;
  }

  // Right-click the request pane: send elsewhere / copy URL, cURL, body, raw.
  function openReqMenu(e) {
    e.preventDefault();
    activePane = 'req';
    const body = splitMessage(tab.raw || '').body;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: 'Save to Organizer', hint: store.organizerTargetHint, run: () => store.saveRepeaterToOrganizer(tab) },
        { label: 'Send to Intruder', run: () => store.sendToIntruder({ target: tab.target, raw: tab.raw }) },
        { label: 'Send to Decoder', run: () => store.sendStringToDecoder(tab.raw || '') },
        { label: 'Send to Comparer', run: () => store.addToComparer(tab.raw || '', `${tab.name || 'Repeater'} req`, 'req') },
        'separator',
        { label: 'Copy URL', run: () => store.copyText(requestUrl(), 'URL') },
        { label: 'Copy as cURL', run: () => store.copyText(toCurl(tab.raw || '', tab.target), 'cURL command') },
        { label: 'Copy request body', disabled: !body, run: () => store.copyText(body, 'Request body') },
        { label: 'Copy raw request', run: () => store.copyText(tab.raw || '', 'Request') },
        // Same label and the same place in the menu as History's, and the same action as
        // the camera button in the toolbar above: one capture of the request AND the
        // response, taken from where each pane is scrolled to.
        { label: 'Capture image', run: openCapture },
      ],
    };
  }

  // Right-click the response pane: copy body / raw / send to Decoder (+ request shortcuts).
  function openResMenu(e) {
    activePane = 'res';
    const respRaw = !tab.error ? tab.response?.responseRaw : '';
    if (!respRaw) return; // nothing captured yet: leave the native menu
    e.preventDefault();
    const body = splitMessage(respRaw).body;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: 'Copy response body', disabled: !body, run: () => store.copyText(body, 'Response body') },
        { label: 'Copy raw response', run: () => store.copyText(respRaw, 'Response') },
        { label: 'Send response to Decoder', run: () => store.sendStringToDecoder(respRaw) },
        { label: 'Send response to Comparer', run: () => store.addToComparer(respRaw, `${tab.name || 'Repeater'} res`, 'res') },
        'separator',
        { label: 'Save to Organizer', hint: store.organizerTargetHint, run: () => store.saveRepeaterToOrganizer(tab) },
        { label: 'Send request to Intruder', run: () => store.sendToIntruder({ target: tab.target, raw: tab.raw }) },
        { label: 'Copy URL', run: () => store.copyText(requestUrl(), 'URL') },
        { label: 'Copy as cURL', run: () => store.copyText(toCurl(tab.raw || '', tab.target), 'cURL command') },
        { label: 'Capture image', run: openCapture },
      ],
    };
  }

  let groupNames = $derived(groups.map((g) => g.name).filter(Boolean));

  // Shared "+ Header" / "+ Body" dropdowns (see editormenu.js), so Repeater and
  // Intercept offer exactly the same actions.
  const rawGet = () => tab.raw;
  const rawSet = (v) => (tab.raw = v);
  function openHeaderMenu(e) {
    const r = e.currentTarget.getBoundingClientRect();
    menu = { x: r.left, y: r.bottom + 4, items: headerMenuItems(rawGet, rawSet) };
  }
  function openBodyMenu(e) {
    const r = e.currentTarget.getBoundingClientRect();
    menu = { x: r.left, y: r.bottom + 4, items: bodyMenuItems(rawGet, rawSet) };
  }

  const SWATCH_HEX = { red: '#ff5c6c', orange: '#ff9345', yellow: '#f2c94c', green: '#3ecf8e', cyan: '#38c7d6', blue: '#4f9dff', purple: '#9b7bff', pink: '#f06bb8', gray: '#8a92a6' };
  const groupColor = (name) => rep.groupColors?.[name] || 'var(--accent)';

  // A brand-new group takes the first swatch no other group is using, so two groups
  // made one after another never look alike. Past the ninth it wraps, which is the
  // point where the colours stop carrying meaning anyway.
  const PALETTE = HIGHLIGHTS.map((c) => SWATCH_HEX[c]);
  function nextGroupColor() {
    const used = new Set(Object.values(rep.groupColors ?? {}));
    return PALETTE.find((hex) => !used.has(hex)) ?? PALETTE[Object.keys(rep.groupColors ?? {}).length % PALETTE.length];
  }

  /** Which tabs a group shows: a collapsed one still shows the tab you are editing. */
  const shownTabs = (g) => (!g.name || !isCollapsed(g.name) ? g.tabs : g.tabs.filter((t) => t.id === tab?.id));

  function setGroupColor(name, hex) {
    rep.groupColors = { ...rep.groupColors, [name]: hex };
  }
  function renameGroup(oldName, raw) {
    const name = raw.trim();
    if (!name || name === oldName) return;
    for (const t of rep.tabs) if ((t.group || '') === oldName) t.group = name;
    if (rep.groupColors?.[oldName]) {
      const gc = { ...rep.groupColors, [name]: rep.groupColors[oldName] };
      delete gc[oldName];
      rep.groupColors = gc;
    }
    if (isCollapsed(oldName)) {
      rep.collapsedGroups = [...rep.collapsedGroups.filter((n) => n !== oldName), name];
    }
  }
  function ungroupAll(name) {
    for (const t of rep.tabs) if ((t.group || '') === name) t.group = '';
    dropGroupColor(name);
  }
  async function deleteGroup(name) {
    const ids = rep.tabs.filter((t) => (t.group || '') === name).map((t) => t.id);
    const ok = await store.confirm({
      title: `Delete group "${name}" and its ${ids.length} tab${ids.length === 1 ? '' : 's'}?`,
      message: 'The tabs close with their requests and send history. Use "Ungroup (keep tabs)" to keep them. This cannot be undone.',
    });
    if (!ok) return;
    for (const id of ids) store.closeRepeaterTab(id);
    dropGroupColor(name);
  }
  function dropGroupColor(name) {
    if (!rep.groupColors?.[name]) return;
    const gc = { ...rep.groupColors };
    delete gc[name];
    rep.groupColors = gc;
  }

  function openGroupMenu(e, name) {
    e.preventDefault();
    e.stopPropagation();
    const count = rep.tabs.filter((t) => (t.group || '') === name).length;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: `Send all sequentially (${count})`, hint: '1·2·3', run: () => store.sendGroup(name, 'sequential') },
        { label: `Send all in parallel (${count})`, hint: '⇉', run: () => store.sendGroup(name, 'parallel') },
        'separator',
        { label: 'Rename group…', run: () => (groupModal = { mode: 'rename', oldName: name, name }) },
        {
          label: 'Colour',
          swatches: HIGHLIGHTS.map((c) => ({ color: c, on: rep.groupColors?.[name] === SWATCH_HEX[c], run: () => setGroupColor(name, SWATCH_HEX[c]) })),
        },
        { label: 'Default colour', run: () => dropGroupColor(name) },
        'separator',
        { label: 'Ungroup (keep tabs)', run: () => ungroupAll(name) },
        { label: 'Delete group + tabs…', danger: true, run: () => deleteGroup(name) },
      ],
    };
  }

  // Editing the request while viewing a past send detaches it into a fresh,
  // free-form draft (programmatic history nav sets raw === the entry, so it
  // does not trigger here).
  $effect(() => {
    const raw = tab.raw;
    if (tab.pos !== null && tab.history[tab.pos] && raw !== tab.history[tab.pos].raw) {
      tab.draft = null;
      tab.pos = null;
    }
  });

  function toggleCollapse(name) {
    rep.collapsedGroups = isCollapsed(name)
      ? rep.collapsedGroups.filter((n) => n !== name)
      : [...rep.collapsedGroups, name];
  }

  /** Move a tab into `group`, optionally before `beforeId`, and reorder the list. */
  function moveTab(id, group, beforeId = null) {
    const arr = rep.tabs;
    const i = arr.findIndex((t) => t.id === id);
    if (i === -1) return;
    const [t] = arr.splice(i, 1);
    t.group = group;
    if (beforeId != null && beforeId !== id) {
      arr.splice(arr.findIndex((x) => x.id === beforeId), 0, t);
    } else {
      let last = -1;
      arr.forEach((x, idx) => ((x.group || '') === (group || '') ? (last = idx) : null));
      arr.splice(last + 1, 0, t);
    }
  }

  // The last response is a redirect we can follow (3xx + Location).
  let canFollow = $derived(!!(tab.response && !tab.error && redirectInfo(tab.response.responseRaw)));

  // When the active tab is in a group, the Send button offers group-wide sends.
  function openSendMenu(e) {
    const r = e.currentTarget.getBoundingClientRect();
    const name = tab.group;
    const count = rep.tabs.filter((t) => (t.group || '') === name).length;
    menu = {
      x: r.right - 220,
      y: r.bottom + 4,
      items: [
        { label: 'Send this tab', hint: '⌘↵', run: () => store.sendRepeater(tab) },
        'separator',
        { label: `Sync send - one by one (${count})`, hint: '1·2·3', run: () => store.sendGroup(name, 'sequential') },
        { label: `Send multi - all at once (${count})`, hint: '⇉', run: () => store.sendGroup(name, 'parallel') },
      ],
    };
  }

  // Send-mode menu for a whole group (from the ▶ on the group label).
  function openGroupSend(e, name) {
    e.stopPropagation();
    const r = e.currentTarget.getBoundingClientRect();
    const count = rep.tabs.filter((t) => (t.group || '') === name).length;
    menu = {
      x: r.left,
      y: r.bottom + 4,
      items: [
        { label: `Sync send - one by one (${count})`, hint: '1·2·3', run: () => store.sendGroup(name, 'sequential') },
        { label: `Send multi - all at once (${count})`, hint: '⇉', run: () => store.sendGroup(name, 'parallel') },
      ],
    };
  }

  function openMenu(e, t) {
    e.preventDefault();
    rep.activeId = t.id;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: 'Rename', run: () => (renaming = t.id) },
        { label: 'Duplicate', run: () => store.duplicateRepeaterTab(t) },
        { label: 'Save to Organizer', hint: store.organizerTargetHint, run: () => store.saveRepeaterToOrganizer(t) },
        'separator',
        { label: 'New group…', run: () => (groupModal = { mode: 'new', tabId: t.id, name: '' }) },
        ...groupNames
          .filter((n) => n !== t.group)
          .map((n) => ({ label: `Move to "${n}"`, run: () => moveTab(t.id, n) })),
        ...(t.group ? [{ label: 'Ungroup', run: () => moveTab(t.id, '') }] : []),
        'separator',
        { label: 'Close tab', danger: true, run: () => store.closeRepeaterTab(t.id) },
      ],
    };
  }

  function confirmGroupModal() {
    const name = groupModal?.name.trim();
    if (name) {
      if (groupModal.mode === 'new') {
        // Freshness has to be read before the move, because `groupNames` is derived
        // from the tabs: after moveTab the name exists either way.
        const fresh = !groupNames.includes(name);
        const colour = fresh ? nextGroupColor() : null;
        moveTab(groupModal.tabId, name);
        if (fresh) {
          setGroupColor(name, colour);
          // New groups start closed. The tab you were on stays visible inside the
          // collapsed group (see shownTabs), so grouping never hides your own work.
          if (!isCollapsed(name)) rep.collapsedGroups = [...(rep.collapsedGroups ?? []), name];
        }
      } else renameGroup(groupModal.oldName, name);
    }
    groupModal = null;
  }

  // Pointer-based drag & drop (HTML5 DnD is unreliable inside the webview).
  // Drag a tab onto a group label to add it, onto another tab to move it there,
  // or anywhere else in the tab bar to take it out of its group.
  let dragId = $state(null); // tab being dragged (for styling)
  let dragOver = $state(null); // current drop target key (for highlight)
  let drag = null; // { id, startX, startY, active }

  function dropTargetAt(x, y) {
    const el = document.elementFromPoint(x, y);
    if (!el) return null;
    const groupEl = el.closest('[data-group]');
    if (groupEl) return { kind: 'group', group: groupEl.dataset.group, key: `g:${groupEl.dataset.group}` };
    const tabEl = el.closest('[data-tabid]');
    if (tabEl) {
      const id = Number(tabEl.dataset.tabid);
      return { kind: 'tab', id, group: tabEl.dataset.groupOf || '', key: `t:${id}` };
    }
    if (el.closest('.tabs')) return { kind: 'ungroup', key: 'ungroup' };
    return null;
  }

  function dragDown(e, t) {
    if (e.button !== 0) return;
    if (e.target.closest?.('.close') || e.target.closest?.('.rename')) return;
    drag = { id: t.id, startX: e.clientX, startY: e.clientY, active: false };
    e.currentTarget.setPointerCapture?.(e.pointerId);
  }
  function dragMove(e) {
    if (!drag) return;
    if (!drag.active && Math.hypot(e.clientX - drag.startX, e.clientY - drag.startY) > 4) {
      drag.active = true;
      dragId = drag.id;
    }
    if (drag.active) {
      const target = dropTargetAt(e.clientX, e.clientY);
      dragOver = target && target.key !== `t:${drag.id}` ? target.key : null;
    }
  }
  function dragUp(e) {
    if (!drag) return;
    const d = drag;
    drag = null;
    dragId = null;
    dragOver = null;
    if (!d.active) return; // a click, not a drag: let onclick select the tab
    const target = dropTargetAt(e.clientX, e.clientY);
    if (!target) return;
    if (target.kind === 'group') moveTab(d.id, target.group);
    else if (target.kind === 'tab') moveTab(d.id, target.group, target.id !== d.id ? target.id : null);
    else moveTab(d.id, '');
  }

  // Reflect how far the user scrolled each pane, so a long request/response is
  // captured from where they are looking, not always from the top.
  function openCapture() {
    const first = (tab.raw || '').split('\n')[0] || '';
    const method = first.split(' ')[0] || '';
    let host = tab.target;
    try { host = new URL(tab.target).host; } catch { /* keep */ }
    // Visible scroll fraction of the request editor (its scroller is `.scroll`), for
    // Capture's "follow page scroll".
    const reqEl = document.querySelector('.repeater .scroll');
    const reqFrac = reqEl && reqEl.scrollHeight > reqEl.clientHeight + 2
      ? { top: reqEl.scrollTop / reqEl.scrollHeight, bottom: (reqEl.scrollTop + reqEl.clientHeight) / reqEl.scrollHeight }
      : null;
    const respRaw = tab.error ? tab.error : (tab.response?.responseRaw ?? '');
    capture = {
      method,
      host,
      url: requestUrl(),
      status: tab.response?.status,
      durationMs: tab.response?.durationMs,
      title: tab.name || '',
      // Raw text (for the Beautify toggles) + the view the UI shows (for Follow page scroll).
      request: tab.raw,
      response: respRaw,
      requestView: tab.raw,
      responseView: resRef?.displayText?.() || respRaw,
      reqFrac,
      resFrac: resRef?.visibleFraction?.() ?? null,
    };
  }
  let beautified = $derived(prettyBody(tab.raw));

  // Auto-beautify a tab's request body the first time it's shown (JSON / XML / HTML only;
  // a body that can't be prettified is left exactly as typed, so odd / smuggling payloads
  // are never touched). Once per tab, and only on the live draft (pos === null), so it
  // never disturbs a pinned history view or fights later manual edits.
  const autoPrettied = new Set();
  $effect(() => {
    const id = tab.id;
    if (autoPrettied.has(id)) return;
    autoPrettied.add(id);
    if (tab.pos === null && beautified && beautified !== tab.raw) tab.raw = beautified;
  });

  // Tabs clustered by their group name (ungrouped first, then first-seen order).
  let groups = $derived.by(() => {
    const order = [];
    const map = new Map();
    for (const t of rep.tabs) {
      const g = t.group || '';
      if (!map.has(g)) {
        map.set(g, []);
        order.push(g);
      }
      map.get(g).push(t);
    }
    return order.map((name) => ({ name, tabs: map.get(name) }));
  });

  // Persist tab names/targets/requests whenever they change.
  $effect(() => store.saveRepeater());

  // History of sent requests, loaded the first time a tab is shown.
  $effect(() => {
    if (!tab.historyLoaded) store.loadRepeaterHistory(tab);
  });
  let canBack = $derived(tab.history.length > 0 && (tab.pos === null || tab.pos > 0));
  let canForward = $derived(tab.pos !== null && (tab.pos < tab.history.length - 1 || !!tab.draft));
  let shownEntry = $derived(tab.pos === null ? null : tab.history[tab.pos]);
  // When the request/response was sent: the browsed entry, else the latest send.
  let currentTs = $derived(tab.pos !== null ? tab.history[tab.pos]?.ts : tab.history.at(-1)?.ts);
  // At the live edit (pos === null) show the newest position (N/N) rather than -/N.
  let posLabel = $derived(
    tab.pos !== null
      ? `${tab.pos + 1}/${tab.history.length}`
      : tab.history.length
      ? `${tab.history.length}/${tab.history.length}`
      : '-',
  );
  let posTitle = $derived(
    shownEntry
      ? `Sent ${fmtDateTime(shownEntry.ts)} to ${shownEntry.target}`
      : tab.history.length
      ? `${tab.history.length} requests sent; use < to review them`
      : 'No requests sent from this tab yet',
  );

  // ---- socket tabs ---------------------------------------------------------------
  //
  // A socket tab is not a request, so it gets its own toolbar rather than borrowing the
  // one above a request editor. Sharing it is what made the panel feel broken: the big
  // primary button said Send, pressing it answered "request is empty", and the real
  // Connect was a secondary button further down the page.
  let isWs = $derived(tab?.kind === 'ws');

  // An HTTP tab that is really a socket. It happens the moment a handshake reaches the
  // Repeater by some route that did not know better, and the tab it makes cannot work:
  // ws:// is not an address the HTTP client can dial, and a 101 cannot be replayed
  // anyway. Offered, not forced - sending an `Upgrade: websocket` header over plain
  // HTTP to see what a server does with it is a real test.
  let socketish = $derived(!isWs && (isSocketUrl(tab?.target) || isHandshakeRequest(tab?.raw)));

  // Totals come from the frame list, tagged with the flow they were counted for, so
  // switching tabs can never show the previous socket's numbers.
  let counted = $state(null);
  let stats = $derived(counted?.flowId === tab?.flowId ? counted : { total: 0, up: 0, down: 0, bytes: 0 });

  let wsState = $derived(tab?.connId ? 'open' : tab?.wsState === 'connecting' ? 'connecting' : (tab?.wsState ?? 'idle'));
  let wsStateText = $derived(
    {
      open: 'open',
      connecting: 'connecting…',
      closed: tab?.closedBy === 'server' ? 'closed by the server' : tab?.closedBy === 'you' ? 'closed' : 'closed',
      idle: 'not connected',
    }[wsState] ?? 'not connected',
  );

  const FRAME_KINDS = [
    { id: 'text', label: 'Text', hint: 'UTF-8, sent exactly as typed' },
    { id: 'binary', label: 'Binary', hint: 'hex bytes: 89 50 4e 47' },
    { id: 'ping', label: 'Ping', hint: 'a control frame; payload optional, 125 bytes max' },
    { id: 'pong', label: 'Pong', hint: 'a control frame; payload optional, 125 bytes max' },
  ];
  let payloadEl = $state();
  let frameKind = $derived(tab?.frameKind ?? 'text');
  let payloadBytes = $derived(new TextEncoder().encode(tab?.payload ?? '').length);
  let hexDigits = $derived(String(tab?.payload ?? '').replace(/[^0-9a-f]/gi, '').length);
  // What the composer says about what you typed, before you send it. A control frame
  // over 125 bytes and an odd number of hex digits are both rejected by the socket, so
  // they are called out here instead of coming back as an error afterwards.
  let composeNote = $derived.by(() => {
    if (!isWs) return null;
    if (frameKind === 'binary') {
      if (hexDigits % 2) return { bad: true, text: 'odd number of hex digits' };
      return { bad: false, text: `${hexDigits / 2} bytes` };
    }
    if (frameKind === 'ping' || frameKind === 'pong') {
      return { bad: payloadBytes > 125, text: `${payloadBytes}/125 bytes` };
    }
    return { bad: false, text: `${payloadBytes} bytes` };
  });
  let canFormat = $derived.by(() => {
    if (!isWs || frameKind !== 'text') return false;
    const t = (tab?.payload ?? '').trim();
    if (!t.startsWith('{') && !t.startsWith('[')) return false;
    try {
      JSON.parse(t);
      return true;
    } catch {
      return false;
    }
  });

  /** Re-indent the composed JSON (never re-serialised: see prettyJsonText). */
  function formatPayload() {
    tab.payload = prettyJsonText(tab.payload) ?? tab.payload;
  }

  /** "Edit & resend" from the frame list: load that frame into the composer. */
  function loadFrame({ kind, payload }) {
    tab.frameKind = kind;
    tab.payload = payload;
    payloadEl?.focus();
  }

  function onKey(e) {
    // One handler, on the window, for the whole view - including the socket composer.
    // The composer used to carry its own Cmd+Enter as well, so the shortcut fired twice:
    // once as a frame and once here as an HTTP send, which is where the red
    // "request is empty" under a socket tab came from. `sendRepeater` now routes a
    // socket tab to its composer, so there is one path and one frame.
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault();
      store.sendRepeater(tab);
    } else if ((e.metaKey || e.ctrlKey) && (e.key === 'r' || e.key === 'R')) {
      // Cmd/Ctrl+R duplicates the active tab (and never reloads the webview).
      e.preventDefault();
      store.duplicateRepeaterTab(tab);
    }
  }

  // The host[:port] of a target URL, e.g. "https://ex.com:8443/x" -> "ex.com:8443".
  function hostOf(target) {
    const s = (target || '').trim();
    if (!s) return '';
    for (const url of [s, `http://${s}`]) {
      try {
        return new URL(url).host;
      } catch {
        // try the next form
      }
    }
    return '';
  }

  // Editing the target reflects into the Host header (one way only - editing the
  // Host header in the request does NOT change the target). Fires on manual edits.
  function reflectHostFromTarget(e) {
    const host = hostOf(e.currentTarget.value);
    if (host) tab.raw = setHeader(tab.raw, 'Host', host);
  }

  function focusSelect(node) {
    node.focus();
    node.select();
  }

  function finishRename(t, value) {
    const name = value.trim();
    if (name) t.name = name;
    renaming = null;
  }

  function close(e, id) {
    e.stopPropagation();
    store.closeRepeaterTab(id);
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="repeater">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="tabs" class:wrapped={wrapTabs} role="tablist" bind:this={tabsEl} onwheel={onTabsWheel}>
    <button
      class="layout-toggle"
      class:on={wrapTabs}
      title={wrapTabs ? 'Tabs in one scrollable row' : 'Stack tabs on multiple rows'}
      aria-label="Toggle tab layout"
      aria-pressed={wrapTabs}
      onclick={toggleWrap}
    >
      <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        {#if wrapTabs}<path d="M5 12h14M13 6l6 6-6 6" /><path d="M5 6v12" />{:else}<path d="M4 6h16M4 12h16M4 18h16" />{/if}
      </svg>
    </button>
    {#each groups as g (g.name)}
      {#if g.name}
        <div class="group-head" class:drop-on={dragOver === `g:${g.name}`} data-group={g.name} style="--gc: {groupColor(g.name)}">
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <button
            class="group-label"
            title="Group {g.name} - click to collapse, right-click to edit, drop a tab here to add it"
            oncontextmenu={(e) => openGroupMenu(e, g.name)}
            onclick={() => toggleCollapse(g.name)}
          >
            <span class="caret">{isCollapsed(g.name) ? '▸' : '▾'}</span>
            {g.name}
            <span class="gcount">{g.tabs.length}</span>
          </button>
          <button class="group-send" title="Send all in this group (sequential / parallel)" onclick={(e) => openGroupSend(e, g.name)}>▶</button>
        </div>
      {/if}
      {#if shownTabs(g).length}
        {#each shownTabs(g) as t (t.id)}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div
            class="tab"
            class:active={t.id === tab.id}
            class:grouped={g.name}
            class:dragging={t.id === dragId}
            class:drop-on={dragOver === `t:${t.id}`}
            style={g.name ? `--gc: ${groupColor(g.name)}` : ''}
            role="tab"
            tabindex="0"
            aria-selected={t.id === tab.id}
            data-tabid={t.id}
            data-group-of={g.name || ''}
            title="{t.target} - drag to a group (or out), right-click for options"
            onclick={() => (rep.activeId = t.id)}
            onkeydown={(e) => e.key === 'Enter' && (rep.activeId = t.id)}
            ondblclick={() => (renaming = t.id)}
            oncontextmenu={(e) => openMenu(e, t)}
            onauxclick={(e) => e.button === 1 && close(e, t.id)}
            onpointerdown={(e) => dragDown(e, t)}
            onpointermove={dragMove}
            onpointerup={dragUp}
          >
            {#if t.sending}
              <span class="spinner"></span>
            {:else if t.error}
              <span class="dot s-err"></span>
            {:else if t.response}
              <span class="dot {statusClass(t.response.status)}"></span>
            {/if}

            {#if renaming === t.id}
              <input
                class="rename"
                value={t.name}
                use:focusSelect
                onblur={(e) => finishRename(t, e.currentTarget.value)}
                onkeydown={(e) => {
                  e.stopPropagation();
                  if (e.key === 'Enter') finishRename(t, e.currentTarget.value);
                  if (e.key === 'Escape') renaming = null;
                }}
              />
            {:else}
              <span class="name">{t.name}</span>
            {/if}
            {#if t.rawMode}<span class="rawtag" title="Raw mode: extensions are skipped for this tab">RAW</span>{/if}

            <button class="close" aria-label="Close tab" onclick={(e) => close(e, t.id)}>×</button>
          </div>
        {/each}
      {/if}
    {/each}
    <div class="dropzone" class:drop-on={dragOver === 'ungroup'} data-ungroup title="Drop here to take a tab out of its group"></div>
    <button class="add" title="New tab" aria-label="New tab" onclick={() => store.addRepeaterTab()}>+</button>
  </div>

  {#if isWs}
    <!-- A socket's toolbar: the address, one button that changes what it is, and the
         state of the connection. Nothing here is borrowed from the request toolbar. -->
    <div class="toolbar wstoolbar">
      <input
        class="input mono target"
        bind:value={tab.url}
        placeholder="ws://127.0.0.1:6677/ws/notify"
        spellcheck="false"
        disabled={!!tab.connId}
        title="The socket to open. ws:// or wss:// - an http(s):// address is read as its socket equivalent, so a URL pasted out of History works."
        onkeydown={(e) => e.key === 'Enter' && !tab.connId && store.connectWsTab(tab)}
      />
      {#if tab.connId}
        <button class="btn disconnect" title="Send a close frame and hang up" onclick={() => store.closeWsTab(tab)}>Disconnect</button>
        <label class="codefield" title="The close code sent when you disconnect. 1000 is a normal close; 4000-4999 are yours to use.">
          code
          <input class="input mono code" type="number" min="1000" max="4999" bind:value={tab.closeCode} />
        </label>
      {:else}
        <button class="btn primary connect" disabled={tab.sending || !tab.url.trim()} onclick={() => store.connectWsTab(tab)}>
          {tab.sending ? 'Connecting…' : 'Connect'}
          <kbd>↵</kbd>
        </button>
      {/if}

      <span class="wsstate {wsState}" title={tab.flowId ? `Recorded as flow #${tab.flowId}` : 'No socket has been opened from this tab yet'}>
        <span class="statedot"></span>{wsStateText}
      </span>
      {#if tab.flowId}
        <span class="dim meta" title="Frames sent and received on this socket, and the payload bytes they carry">
          #{tab.flowId} · ↑{stats.up} ↓{stats.down} · {fmtSize(stats.bytes)}
        </span>
      {/if}

      <div class="spacer"></div>
      <button class="btn ghost sm icbtn note-btn" class:has={!!tab.note} title="Note for this tab, its group, or all tabs (double-click a tab to rename)" aria-label="Note" onclick={openNotes}>
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z" /></svg>
      </button>
      <button class="btn ghost sm icbtn" title="Duplicate this tab (the address and the frame, not the connection)" aria-label="Duplicate tab" onclick={() => store.duplicateRepeaterTab(tab)}>
        <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
      </button>
    </div>
  {:else}
  <div class="toolbar">
    <input class="input mono target" bind:value={tab.target} placeholder="https://example.com" spellcheck="false"
      title="The destination. Editing it updates the Host header; the Host header can still be changed on its own."
      oninput={reflectHostFromTarget} />
    <div class="send-split">
      <button class="btn primary send-main" onclick={() => store.sendRepeater(tab)} disabled={tab.sending}>
        {tab.sending ? 'Sending…' : 'Send'}
        <kbd>⌘↵</kbd>
      </button>
      {#if tab.group}
        <button class="btn primary send-caret" title="Group send options ({tab.group})" onclick={openSendMenu} disabled={tab.sending}>▾</button>
      {/if}
    </div>
    {#if tab.sending}
      <button class="btn ghost sm cancel-send" title="Cancel this send (if it's taking too long)" onclick={() => store.cancelRepeater(tab)}>Cancel</button>
    {/if}
    {#if socketish}
      <button
        class="btn ghost sm tosocket"
        title="This request is a WebSocket handshake. Open the same address as a socket instead: frames, not one request."
        onclick={() => store.openTabAsSocket(tab)}
      >
        ⇅ Open as socket
      </button>
    {/if}
    {#if canFollow}
      <button class="btn ghost sm" title="Follow the redirect (Location) and send it" onclick={() => store.followRedirect(tab)} disabled={tab.sending}>Follow redirect →</button>
    {/if}
    <div class="history-nav" role="group" aria-label="Request history">
      <button
        class="btn sm step"
        title="Previous request"
        aria-label="Previous request"
        disabled={!canBack || tab.sending}
        onclick={() => store.repeaterGo(tab, -1)}
      >
        ‹
      </button>
      <span class="pos mono" title={posTitle}>{posLabel}</span>
      {#if shownEntry}<span class="pos-date mono" title={posTitle}>{fmtDateTime(shownEntry.ts)}</span>{/if}
      <button
        class="btn sm step"
        title={tab.pos === tab.history.length - 1 && tab.draft ? 'Back to the unsent edit' : 'Next request'}
        aria-label="Next request"
        disabled={!canForward || tab.sending}
        onclick={() => store.repeaterGo(tab, 1)}
      >
        ›
      </button>
    </div>
    {#if tab.response && !tab.error}
      <span class="badge {statusClass(tab.response.status)}">{tab.response.status}</span>
      <span class="dim meta">{tab.response.durationMs} ms · {fmtSize(tab.response.length)}{#if currentTs} · {fmtDateTime(currentTs)}{/if}</span>
    {/if}
    <div class="spacer"></div>
    {#if store.extActiveOn('repeater')}
      <label class="rawmode" class:on={tab.rawMode} title="Raw mode: skip extensions for this tab. The request leaves exactly as typed and the response is shown exactly as it arrived (no decode, no re-encode).">
        <input type="checkbox" bind:checked={tab.rawMode} />
        Raw
      </label>
    {/if}
    <button class="btn ghost sm icbtn note-btn" class:has={!!tab.note} title="Note for this tab, its group, or all tabs (double-click a tab to rename)" aria-label="Note" onclick={openNotes}>
      <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3a2.828 2.828 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5L17 3z" /></svg>
    </button>
    <button class="btn ghost sm icbtn" title="Duplicate this tab" aria-label="Duplicate tab" onclick={() => store.duplicateRepeaterTab(tab)}>
      <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
    </button>
    <button class="btn ghost sm icbtn" title="Capture: screenshot the request/response to annotate" aria-label="Capture screenshot" onclick={openCapture}>
      <svg viewBox="0 0 24 24" width="15" height="15" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z" /><circle cx="12" cy="13" r="4" /></svg>
    </button>
  </div>

  {/if}

  <div class="grid" class:with-decoder={miniDecode}>
    {#if isWs}
      <!-- A socket tab: the frames fill the panel and the composer sits under them,
           which is the shape of every chat-like thing people already know. The list is
           the ordinary WebSocket viewer pointed at the flow this socket was recorded
           as, so nothing here had to learn how to render a frame. -->
      <div class="rpanes wsrep">
        {#if tab.error}
          <div class="wserr">
            <span>{tab.error}</span>
            <button class="wserr-x" aria-label="Dismiss" onclick={() => (tab.error = null)}>×</button>
          </div>
        {/if}

        <div class="wsframes">
          {#if tab.flowId}
            <WsView
              flowId={tab.flowId}
              handshake={tab.handshake}
              onstats={(s) => (counted = { ...s, flowId: tab.flowId })}
              onreuse={loadFrame}
              empty={tab.connId ? 'Connected. Nothing has come through yet - send a frame below, or wait for the server.' : 'This socket is closed. Its frames are kept.'}
            />
          {:else}
            <div class="panel wsblank">
              <div class="blank-in">
                <h3>Open a socket</h3>
                <p>
                  Type a <code>ws://</code> or <code>wss://</code> address above and press
                  <strong>Connect</strong>, or send a frame and it will connect first.
                </p>
                <p class="dim">
                  The socket is recorded as a flow, so its frames show up in History, can be saved to
                  the Organizer and are readable over MCP - the same as traffic the proxy saw.
                </p>
                <p class="dim">Already captured one? Right-click a 101 row in History → Open socket in Repeater.</p>
              </div>
            </div>
          {/if}
        </div>

        <div class="wscompose">
          <div class="kinds" role="group" aria-label="Frame kind">
            {#each FRAME_KINDS as k (k.id)}
              <button class="seg" class:on={frameKind === k.id} title={k.hint} onclick={() => (tab.frameKind = k.id)}>{k.label}</button>
            {/each}
            {#if composeNote}
              <span class="note mono" class:bad={composeNote.bad}>{composeNote.text}</span>
            {/if}
            <div class="spacer"></div>
            <button class="btn ghost sm" disabled={!canFormat} title="Re-indent the JSON you are composing" onclick={formatPayload}>{'{ }'} Format</button>
            {#if tab.payload}
              <button class="btn ghost sm" title="Clear the composer" onclick={() => (tab.payload = '')}>Clear</button>
            {/if}
          </div>
          <div class="composerow">
            <textarea
              class="input mono payload"
              bind:this={payloadEl}
              bind:value={tab.payload}
              placeholder={frameKind === 'binary'
                ? '89 50 4e 47 0d 0a 1a 0a'
                : frameKind === 'text'
                ? '{"subscribe":"payments"}'
                : 'optional payload'}
              spellcheck="false"
            ></textarea>
            <button
              class="btn primary sendframe"
              disabled={tab.sending || composeNote?.bad || !tab.url.trim()}
              title={tab.connId ? 'Send one frame (Cmd/Ctrl+Enter)' : 'Connect this socket and send the frame (Cmd/Ctrl+Enter)'}
              onclick={() => store.sendWsFrame(tab)}
            >
              {tab.connId ? 'Send' : tab.sending ? 'Connecting…' : 'Connect & send'}
              <kbd>⌘↵</kbd>
            </button>
          </div>
        </div>
      </div>
    {:else}
      <div class="rpanes">
        <SplitPane storeKey="repeater" left={reqPane} right={resPane} />
      </div>
    {/if}
    {#if miniDecode}
      <aside class="decoder-dock"><MiniDecoder text={miniDecode.text} onclose={() => (miniDecode = null)} /></aside>
    {/if}
    {#snippet reqPane()}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="panel" oncontextmenu={openReqMenu} onpointerdowncapture={() => (activePane = 'req')} onfocusincapture={() => (activePane = 'req')}>
      <header class="panel-head">
        <span class="panel-title">Request</span>
        {#if reqFinal}
          <div class="ext-seg" role="group" aria-label="Request view">
            <span class="ext-badge" title="A repeater-scope extension changed the request before sending">ext</span>
            <button class="seg" class:on={reqView === 'typed'} onclick={() => pickReqView('typed')}>Typed</button>
            <button class="seg" class:on={reqView === 'sent'} title="What was actually sent after the extension" onclick={() => pickReqView('sent')}>Sent</button>
          </div>
        {/if}
        <div class="spacer"></div>
        {#if reqView !== 'sent'}
          <button class="btn ghost sm" title="Add a header (static, random User-Agent, spoof IP, host tricks…)" onclick={openHeaderMenu}>+ Header ▾</button>
          <button class="btn ghost sm" title="Insert a body skeleton (JSON, form, XML…) with Content-Type and Content-Length" onclick={openBodyMenu}>+ Body ▾</button>
          <button
            class="btn ghost sm"
            title="Format body JSON / XML / HTML"
            disabled={!beautified || beautified === tab.raw}
            onclick={() => (tab.raw = beautified)}
          >
            Beautify
          </button>
        {/if}
      </header>
      {#if reqView === 'sent' && reqFinal}
        <pre class="code sent-req">{reqFinal}</pre>
      {:else}
        {#key tab.id}
          <CodeEditor bind:this={reqRef} bind:value={tab.raw} onselect={(t) => (miniDecode = { text: t })} />
        {/key}
      {/if}
    </div>
    {/snippet}
    {#snippet resPane()}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="res-wrap" oncontextmenu={openResMenu} onpointerdowncapture={() => (activePane = 'res')} onfocusincapture={() => (activePane = 'res')}>
      {#if tab.error}
        <div class="panel">
          <header class="panel-head"><span class="panel-title">Response</span></header>
          <div class="error">{tab.error}</div>
        </div>
      {:else}
        {#if resOriginal}
          <div class="ext-seg res" role="group" aria-label="Response view">
            <span class="ext-badge" title="A repeater-scope extension transformed the response">ext</span>
            <button class="seg" class:on={resView === 'final'} title="After the extension (e.g. decrypted)" onclick={() => pickResView('final')}>Final</button>
            <button class="seg" class:on={resView === 'original'} title="As it arrived on the wire" onclick={() => pickResView('original')}>Original</button>
          </div>
        {/if}
        <MessageView
          bind:this={resRef}
          title="Response"
          preferPretty
          raw={resView === 'original' && resOriginal ? resOriginal : tab.response?.responseRaw}
          image={resView === 'original' && resOriginal ? null : tab.response?.responseImage}
          ondecode={(t) => (miniDecode = { text: t })}
          placeholder={tab.loadingEntry ? 'Loading response…' : 'Press Send (Cmd+Enter) to send the request'} />
      {/if}
    </div>
    {/snippet}
  </div>
  <div class="foot">
    {#if isWs}
      <span class="foot-hint">Cmd/Ctrl+Enter sends · click a frame to read it, ↻ to send it again</span>
      <div class="spacer"></div>
      {#if stats.total}
        <span class="foot-stat" title="Frames on this socket">{stats.total} frames</span>
      {/if}
    {:else}
    <button class="btn ghost sm" title="Find in the active pane (Cmd/Ctrl+F)" onclick={openFind}>⌕ Find</button>
    <span class="foot-hint">{activePane === 'req' ? 'Request' : 'Response'} · Cmd/Ctrl+F</span>
    <div class="spacer"></div>
    {#if tab.response && !tab.error}
      <span class="foot-stat" title="When sent, response time and size">{#if currentTs}{fmtDateTime(currentTs)} · {/if}{tab.response.durationMs} ms · {fmtSize(tab.response.length)}</span>
    {/if}
    {/if}
  </div>
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if groupModal}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="modal-back" onclick={() => (groupModal = null)}>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="modal" onclick={(e) => e.stopPropagation()}>
      <span class="dim">{groupModal.mode === 'rename' ? 'Rename group' : 'New group name'}</span>
      <input
        class="input"
        bind:value={groupModal.name}
        use:focusSelect
        placeholder="e.g. Auth flows"
        onkeydown={(e) => {
          if (e.key === 'Enter') confirmGroupModal();
          if (e.key === 'Escape') groupModal = null;
        }}
      />
      <div class="modal-actions">
        <button class="btn ghost sm" onclick={() => (groupModal = null)}>Cancel</button>
        <button class="btn primary sm" onclick={confirmGroupModal} disabled={!groupModal.name.trim()}>{groupModal.mode === 'rename' ? 'Rename' : 'Create'}</button>
      </div>
    </div>
  </div>
{/if}

{#if capture}
  <CaptureEditor source={capture} onclose={() => (capture = null)} />
{/if}

{#if notes}
  <Modal title="Note" onclose={() => (notes = null)}>
    <div class="note-scope" role="radiogroup" aria-label="Apply note to">
      <label><input type="radio" bind:group={notes.scope} value="tab" /> This tab</label>
      <label><input type="radio" bind:group={notes.scope} value="group" /> This group ({store.repeaterNoteScopeCount('group')})</label>
      <label><input type="radio" bind:group={notes.scope} value="all" /> All tabs ({store.repeaterNoteScopeCount('all')})</label>
    </div>
    <textarea class="note-area code" bind:value={notes.text} placeholder="Write a note for the selected scope…" spellcheck="false"></textarea>
    <div class="note-actions">
      <button class="btn" onclick={saveNotes}>Save</button>
      <button class="btn ghost" onclick={() => (notes = null)}>Cancel</button>
    </div>
  </Modal>
{/if}

<style>
  .repeater {
    display: grid;
    grid-template-rows: auto auto 1fr auto;
    height: 100%;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 10px;
    border-top: 1px solid var(--border);
    background: var(--bg-elev);
  }
  .foot-hint {
    color: var(--text-faint);
    font: 11px var(--mono);
  }
  .tosocket {
    color: var(--accent);
    border-color: color-mix(in srgb, var(--accent) 45%, var(--border));
    white-space: nowrap;
  }
  .tosocket:hover {
    background: var(--accent-soft);
  }
  .cancel-send {
    color: var(--red);
    border-color: color-mix(in srgb, var(--red) 45%, var(--border));
  }
  .cancel-send:hover {
    background: color-mix(in srgb, var(--red) 14%, transparent);
  }
  .foot-stat {
    color: var(--text-dim);
    font: 11.5px var(--mono);
  }
  .tabs {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    padding: 8px 10px 0;
    overflow-x: auto;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elev);
    scrollbar-width: none;
  }
  /* Stacked layout: tabs wrap onto multiple rows instead of scrolling sideways. */
  .tabs.wrapped {
    flex-wrap: wrap;
    overflow-x: visible;
    row-gap: 4px;
    padding-bottom: 4px;
  }
  .layout-toggle {
    position: sticky;
    left: 0;
    z-index: 3;
    flex-shrink: 0;
    display: inline-grid;
    place-items: center;
    width: 26px;
    height: 28px;
    margin-right: 2px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 6px 6px 0 0;
    background: var(--bg-elev);
    color: var(--text-dim);
    cursor: pointer;
  }
  .tabs.wrapped .layout-toggle {
    position: static;
  }
  .layout-toggle:hover {
    color: var(--text);
    border-color: var(--accent);
  }
  .layout-toggle.on {
    color: var(--accent);
    border-color: var(--accent);
  }
  .tab {
    display: flex;
    flex-shrink: 0;
    align-items: center;
    gap: 7px;
    max-width: 200px;
    height: 32px;
    padding: 0 6px 0 12px;
    border: 1px solid transparent;
    border-bottom: 0;
    border-radius: 8px 8px 0 0;
    color: var(--text-dim);
    font-size: 12.5px;
    cursor: default;
    outline: none;
  }
  .tab:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .tab.active {
    position: relative;
    top: 1px;
    border-color: var(--border);
    background: var(--bg);
    color: var(--text);
  }
  .tab.active::before {
    position: absolute;
    top: -1px;
    right: 10px;
    left: 10px;
    height: 2px;
    border-radius: 0 0 2px 2px;
    background: var(--accent);
    content: '';
  }
  .tab:focus-visible {
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .name {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  /* Raw mode: skipping extensions silently changes what is sent, so it is a visible
     toggle in the bar and a badge on the tab, not a hidden preference. */
  .rawmode {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 0 7px;
    height: 24px;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-dim);
    font-size: 12px;
    cursor: pointer;
    user-select: none;
  }
  .rawmode input {
    accent-color: var(--amber);
  }
  .rawmode.on {
    border-color: color-mix(in srgb, var(--amber) 60%, transparent);
    background: color-mix(in srgb, var(--amber) 12%, transparent);
    color: var(--amber);
  }
  .rawtag {
    padding: 0 4px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--amber) 22%, transparent);
    color: var(--amber);
    font: 700 9px/14px var(--sans);
    letter-spacing: 0.04em;
  }
  .rename {
    width: 110px;
    padding: 2px 5px;
    border: 1px solid var(--accent);
    border-radius: 4px;
    outline: none;
    background: var(--bg);
    color: var(--text);
    font: inherit;
  }
  .dot {
    flex-shrink: 0;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: currentColor;
  }
  .spinner {
    flex-shrink: 0;
    width: 10px;
    height: 10px;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .close {
    display: grid;
    flex-shrink: 0;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--text-faint);
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
  }
  .tab:hover .close,
  .tab.active .close {
    opacity: 1;
  }
  .close:hover {
    background: var(--bg-soft);
    color: var(--text);
  }
  .add {
    flex-shrink: 0;
    width: 28px;
    height: 28px;
    margin: 0 0 3px 4px;
    border: 0;
    border-radius: 7px;
    background: none;
    color: var(--text-dim);
    font-size: 17px;
    cursor: pointer;
  }
  .add:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .target {
    width: min(460px, 50%);
  }
  .send-split {
    display: inline-flex;
    align-items: stretch;
  }
  .send-split .send-main {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
  }
  .send-caret {
    padding: 0 8px;
    margin-left: 1px;
    border-top-left-radius: 0;
    border-bottom-left-radius: 0;
  }
  .group-input {
    width: 96px;
    height: 26px;
    font-size: 12px;
  }
  .group-head {
    display: flex;
    align-items: center;
    align-self: center;
    margin: 0 2px 3px 8px;
    border-radius: 6px;
    background: color-mix(in srgb, var(--gc) 16%, transparent);
  }
  .group-head.drop-on {
    outline: 2px solid var(--gc);
    outline-offset: 1px;
  }
  .group-label {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 3px 4px 3px 9px;
    border: 0;
    border-radius: 6px 0 0 6px;
    background: none;
    color: var(--gc);
    font: 600 10.5px var(--sans);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    white-space: nowrap;
    cursor: pointer;
  }
  .group-label .caret {
    font-size: 9px;
  }
  .group-send {
    padding: 3px 7px 3px 4px;
    border: 0;
    border-radius: 0 6px 6px 0;
    background: none;
    color: var(--gc);
    font-size: 9px;
    cursor: pointer;
  }
  .group-send:hover {
    background: color-mix(in srgb, var(--gc) 26%, transparent);
  }
  .gcount {
    padding: 0 5px;
    border-radius: 99px;
    background: color-mix(in srgb, var(--gc) 26%, transparent);
  }
  .dropzone {
    flex: 1;
    min-width: 40px;
    align-self: stretch;
    border-radius: 6px;
  }
  .dropzone.drop-on {
    background: color-mix(in srgb, var(--text-dim) 20%, transparent);
  }
  .tab.dragging {
    opacity: 0.45;
  }
  .tab.drop-on {
    box-shadow: inset 0 -2px 0 var(--accent);
  }
  .modal-back {
    position: fixed;
    inset: 0;
    z-index: 210;
    display: grid;
    place-items: center;
    background: rgb(0 0 0 / 0.4);
  }
  .modal {
    display: grid;
    gap: 10px;
    width: 300px;
    padding: 18px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
    font-size: 12px;
  }
  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .tab.grouped {
    box-shadow: inset 2px 0 0 var(--gc);
  }
  .history-nav {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  .step {
    width: 26px;
    justify-content: center;
    padding: 0;
    font-size: 15px;
    line-height: 1;
  }
  .pos {
    min-width: 44px;
    color: var(--text-dim);
    font-size: 11.5px;
    text-align: center;
  }
  .pos-date {
    color: var(--text-faint);
    font-size: 11px;
    white-space: nowrap;
  }
  .meta {
    font-size: 12px;
  }
  .grid {
    position: relative;
    display: grid;
    grid-template-columns: 1fr;
    min-height: 0;
    padding: 10px;
  }
  /* The decoder takes a column of its own, sized to its (draggable) width, so the panes
     shrink to make room instead of being covered. It used to be `position: absolute`
     over the panes, which hid the response exactly when you wanted to read it next to
     the decoded value. Same shape as History and Organizer. */
  .grid.with-decoder {
    grid-template-columns: minmax(0, 1fr) auto;
  }
  /* ---- socket tab ----
     The panel is a flex column: the frame list takes everything left over and the
     composer keeps its own height. The dead white rectangle this replaced came from a
     layout where the list only appeared once a flow existed, so before connecting the
     pane had nothing in it at all. */
  .wsrep {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .wsframes {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .wsframes > :global(.panel) {
    flex: 1;
    min-height: 0;
  }
  /* The address is the whole subject of this toolbar, so it gets more of the row than
     a request's target does (that row also carries Send, history and status). */
  .wstoolbar .target {
    width: min(620px, 52%);
  }
  .wstoolbar .target:disabled {
    color: var(--text-dim);
    cursor: default;
  }
  .connect kbd,
  .sendframe kbd {
    margin-left: 6px;
  }
  .disconnect {
    color: var(--red);
    border-color: color-mix(in srgb, var(--red) 45%, var(--border));
  }
  .disconnect:hover {
    background: color-mix(in srgb, var(--red) 14%, transparent);
  }
  .codefield {
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--text-faint);
    font-size: 11px;
  }
  .codefield .code {
    width: 72px;
    padding: 0 6px;
    text-align: center;
  }
  /* The connection, in one chip: colour for the state, words for what it means. */
  .wsstate {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 9px;
    height: 24px;
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--text-dim);
    font-size: 11.5px;
    white-space: nowrap;
  }
  .statedot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--text-faint);
  }
  .wsstate.open {
    color: var(--green, #35c08a);
    border-color: color-mix(in srgb, var(--green, #35c08a) 45%, var(--border));
  }
  .wsstate.open .statedot {
    background: var(--green, #35c08a);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--green, #35c08a) 22%, transparent);
  }
  .wsstate.connecting .statedot {
    background: var(--accent);
    animation: pulse 1s ease-in-out infinite;
  }
  .wsstate.closed .statedot {
    background: var(--red);
  }
  @keyframes pulse {
    50% {
      opacity: 0.25;
    }
  }
  .wstoolbar .meta {
    font: 11.5px var(--mono);
    white-space: nowrap;
  }
  .wserr {
    display: flex;
    flex-shrink: 0;
    align-items: center;
    gap: 8px;
    padding: 7px 10px;
    border-bottom: 1px solid color-mix(in srgb, var(--red) 35%, var(--border));
    background: color-mix(in srgb, var(--red) 10%, transparent);
    color: var(--red);
    font-size: 12.5px;
  }
  .wserr span {
    flex: 1;
  }
  .wserr-x {
    border: 0;
    background: none;
    color: inherit;
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
  }
  /* Before the first connection: the panel says what this tab is for instead of
     showing an empty box. */
  .wsblank {
    display: flex;
    align-items: center;
    justify-content: center;
    text-align: center;
  }
  .blank-in {
    max-width: 440px;
    padding: 24px;
  }
  .blank-in h3 {
    margin: 0 0 8px;
    font: 600 14px var(--sans);
  }
  .blank-in p {
    margin: 0 0 8px;
    font-size: 12.5px;
    line-height: 1.6;
  }
  .blank-in code {
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-hover);
    font: 11.5px var(--mono);
  }
  /* The composer. Its height is its own; the list above absorbs the rest. */
  .wscompose {
    flex-shrink: 0;
    border-top: 1px solid var(--border);
    background: var(--bg-elev);
    padding: 8px 10px 10px;
  }
  .kinds {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 7px;
  }
  .kinds .note {
    color: var(--text-faint);
    font-size: 11px;
  }
  .kinds .note.bad {
    color: var(--red);
  }
  .composerow {
    display: flex;
    gap: 8px;
    align-items: stretch;
  }
  .payload {
    flex: 1;
    min-height: 62px;
    max-height: 180px;
    resize: vertical;
    padding: 7px 9px;
    line-height: 1.5;
  }
  .sendframe {
    min-width: 128px;
  }

  .rpanes {
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    min-width: 0;
    min-height: 0;
  }
  /* The decoder FLOATS over the right edge instead of taking a grid column, so
     opening or closing it never reflows (re-wraps) the request/response editors.
     Resizing the editor under the caret is what made placing the cursor go wrong
     after the decoder closed. */
  .decoder-dock {
    min-width: 0;
    min-height: 0;
  }
  .error {
    padding: 14px;
    color: var(--red);
    font-family: var(--mono);
    font-size: 12.5px;
    white-space: pre-wrap;
  }
  /* Wraps the response so the ms/size badge can sit in its bottom-right corner. */
  .res-wrap {
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .res-wrap > :global(.panel) {
    flex: 1;
    min-height: 0;
  }
  .ext-seg {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    margin-left: 8px;
  }
  .ext-seg.res {
    margin: 0 0 6px 0;
  }
  .ext-badge {
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--accent);
    color: #fff;
    font: 600 9.5px var(--sans);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .seg {
    font: 500 11px var(--sans);
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .seg.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .sent-req {
    flex: 1;
    margin: 0;
    overflow: auto;
    padding: 10px 12px;
    font: 12.5px/1.55 var(--mono);
    color: var(--text-dim);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .note-btn.has {
    border-color: var(--accent);
    color: var(--accent);
  }
  .icbtn {
    display: inline-grid;
    place-items: center;
    padding: 0 7px;
  }
  .icbtn svg {
    display: block;
  }
  .note-scope {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    margin-bottom: 10px;
    font-size: 12.5px;
  }
  .note-scope label {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
  }
  .note-scope input {
    accent-color: var(--accent);
  }
  .note-area {
    width: 100%;
    min-height: 160px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    color: var(--text);
    font: 12.5px var(--mono);
    resize: vertical;
    outline: none;
  }
  .note-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
  }
</style>
