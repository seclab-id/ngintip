<script>
  import CodeEditor from './CodeEditor.svelte';
  import MessageView from './MessageView.svelte';
  import MiniDecoder from './MiniDecoder.svelte';
  import SplitPane from './SplitPane.svelte';
  import CaptureEditor from './CaptureEditor.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import Modal from './Modal.svelte';
  import { store } from '../lib/store.svelte.js';
  import { fmtSize, fmtDateTime, statusClass } from '../lib/format.js';
  import { brandKey } from '../lib/brand.js';
  import { prettyBody, splitMessage } from '../lib/message.js';
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
  // Back to the editable/transformed default when the active tab changes.
  $effect(() => {
    tab?.id;
    reqView = 'typed';
    resView = 'final';
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
        { label: 'Send to Intruder', run: () => store.sendToIntruder({ target: tab.target, raw: tab.raw }) },
        { label: 'Send to Decoder', run: () => store.sendStringToDecoder(tab.raw || '') },
        { label: 'Send to Comparer', run: () => store.addToComparer(tab.raw || '', `${tab.name || 'Repeater'} req`, 'req') },
        'separator',
        { label: 'Copy URL', run: () => store.copyText(requestUrl(), 'URL') },
        { label: 'Copy as cURL', run: () => store.copyText(toCurl(tab.raw || '', tab.target), 'cURL command') },
        { label: 'Copy request body', disabled: !body, run: () => store.copyText(body, 'Request body') },
        { label: 'Copy raw request', run: () => store.copyText(tab.raw || '', 'Request') },
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
        { label: 'Send request to Intruder', run: () => store.sendToIntruder({ target: tab.target, raw: tab.raw }) },
        { label: 'Copy URL', run: () => store.copyText(requestUrl(), 'URL') },
        { label: 'Copy as cURL', run: () => store.copyText(toCurl(tab.raw || '', tab.target), 'cURL command') },
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
  function deleteGroup(name) {
    for (const id of rep.tabs.filter((t) => (t.group || '') === name).map((t) => t.id)) store.closeRepeaterTab(id);
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
        { label: 'Delete group + tabs', danger: true, run: () => deleteGroup(name) },
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
        { label: `Sync send — one by one (${count})`, hint: '1·2·3', run: () => store.sendGroup(name, 'sequential') },
        { label: `Send multi — all at once (${count})`, hint: '⇉', run: () => store.sendGroup(name, 'parallel') },
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
        { label: `Sync send — one by one (${count})`, hint: '1·2·3', run: () => store.sendGroup(name, 'sequential') },
        { label: `Send multi — all at once (${count})`, hint: '⇉', run: () => store.sendGroup(name, 'parallel') },
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
      if (groupModal.mode === 'new') moveTab(groupModal.tabId, name);
      else renameGroup(groupModal.oldName, name);
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

  function onKey(e) {
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

  // Editing the target reflects into the Host header (one way only — editing the
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
            title="Group {g.name} — click to collapse, right-click to edit, drop a tab here to add it"
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
      {#if !g.name || !isCollapsed(g.name)}
        {#each g.tabs as t (t.id)}
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

            <button class="close" aria-label="Close tab" onclick={(e) => close(e, t.id)}>×</button>
          </div>
        {/each}
      {/if}
    {/each}
    <div class="dropzone" class:drop-on={dragOver === 'ungroup'} data-ungroup title="Drop here to take a tab out of its group"></div>
    <button class="add" title="New tab" aria-label="New tab" onclick={() => store.addRepeaterTab()}>+</button>
  </div>

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

  <div class="grid" class:with-decoder={miniDecode}>
    <div class="rpanes">
      <SplitPane storeKey="repeater" left={reqPane} right={resPane} />
    </div>
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
            <button class="seg" class:on={reqView === 'typed'} onclick={() => (reqView = 'typed')}>Typed</button>
            <button class="seg" class:on={reqView === 'sent'} title="What was actually sent after the extension" onclick={() => (reqView = 'sent')}>Sent</button>
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
            <button class="seg" class:on={resView === 'final'} title="After the extension (e.g. decrypted)" onclick={() => (resView = 'final')}>Final</button>
            <button class="seg" class:on={resView === 'original'} title="As it arrived on the wire" onclick={() => (resView = 'original')}>Original</button>
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
    <button class="btn ghost sm" title="Find in the active pane (Cmd/Ctrl+F)" onclick={openFind}>⌕ Find</button>
    <span class="foot-hint">{activePane === 'req' ? 'Request' : 'Response'} · Cmd/Ctrl+F</span>
    <div class="spacer"></div>
    {#if tab.response && !tab.error}
      <span class="foot-stat" title="When sent, response time and size">{#if currentTs}{fmtDateTime(currentTs)} · {/if}{tab.response.durationMs} ms · {fmtSize(tab.response.length)}</span>
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
    display: grid;
    grid-template-columns: 1fr;
    min-height: 0;
    padding: 10px;
  }
  .grid.with-decoder {
    /* The dock column sizes to the MiniDecoder's own (draggable) width. */
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 10px;
  }
  .rpanes {
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    min-width: 0;
    min-height: 0;
  }
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
