<script>
  import { tick } from 'svelte';
  import FlowTable from './FlowTable.svelte';
  import MessageView from './MessageView.svelte';
  import WsView from './WsView.svelte';
  import SplitPane from './SplitPane.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import MiniDecoder from './MiniDecoder.svelte';
  import CaptureEditor from './CaptureEditor.svelte';
  import Modal from './Modal.svelte';
  import { store } from '../lib/store.svelte.js';
  import * as api from '../lib/api.js';
  import { pollVisible } from '../lib/poll.js';
  import { HIGHLIGHTS } from '../lib/colors.js';
  import { inScope } from '../lib/scope.js';
  import { HISTORY_COLUMNS, DEFAULT_COLUMN_KEYS } from '../lib/columns.js';
  import { fmtSize, fmtDateTime } from '../lib/format.js';
  import { METHODS, STATUS_CLASSES, FILTER_FIELDS, advActive, matchAdvanced } from '../lib/histfilter.js';
  import { brandKey } from '../lib/brand.js';

  let filtersModal = $state(false);

  // Persist the scope checkbox + advanced filter to the project (debounced). Reading
  // both here synchronously makes this effect re-run on any of their fields changing.
  $effect(() => store.saveHistoryFilter());

  // Keep the "Browser opened" state in sync with whether a launched browser is still
  // running, so the button reverts to "Open browser" once it's closed. Only poll while
  // a browser is actually open (there is nothing to detect otherwise) and the window is
  // visible, so the idle landing screen does zero background IPC.
  $effect(() => {
    if (!store.browser.opened) return;
    return pollVisible(() => {
      api.browserRunning().then((r) => {
        if (store.browser.opened !== r) store.browser = { ...store.browser, opened: r };
      }).catch(() => {});
    }, 3000);
  });

  // In-pane find. The bottom-bar Find button opens search on whichever req/res pane
  // was last interacted with (Cmd/Ctrl+F targets the hovered pane directly).
  let reqRef = $state();
  let resRef = $state();
  let activePane = $state('res');

  // When arriving from a passive finding, highlight its match in the right pane once
  // the flow's detail has loaded (so "Open request in History" points at the match).
  $effect(() => {
    const pf = store.pendingFinding;
    if (!pf || !detail || detail.summary?.id !== pf.flowId) return;
    store.pendingFinding = null; // consume once
    const ref = pf.where === 'request' ? reqRef : resRef;
    // The stored match is truncated for display (…) and whitespace-collapsed; search
    // the leading portion so the highlight lands on the real occurrence.
    const q = String(pf.match || '').replace(/…+$/, '').trim();
    activePane = pf.where === 'request' ? 'req' : 'res';
    tick().then(() => ref?.openFind?.(q));
  });
  function openFind() {
    const ref = activePane === 'req' ? reqRef : resRef;
    ref?.openFind?.();
  }

  let query = $derived(store.filter.trim().toLowerCase());
  let scoped = $derived(
    store.scopeOnly && store.scope.length ? store.flows.filter((f) => inScope(store.scope, f.host)) : store.flows,
  );
  // "#15559" jumps straight to that flow by id (the id as shown in the table), instead
  // of the usual text search. A bare number still searches status/path as before.
  let idQuery = $derived(/^#\d+$/.test(query) ? Number(query.slice(1)) : null);
  // One search box: match summary fields (path/method/status/host/mime/note) OR the
  // request/response body (full-text, via store.bodyHits) — the two are unioned.
  let byText = $derived(
    idQuery != null
      ? scoped.filter((f) => f.id === idQuery)
      : query
        ? scoped.filter(
            (f) =>
              `${f.method} ${f.host}${f.path} ${f.status} ${f.mime} ${f.note} ${f.color}`.toLowerCase().includes(query) ||
              !!store.bodyHits?.has(f.id),
          )
        : scoped,
  );
  let filtered = $derived(advActive(store.adv) ? byText.filter((f) => matchAdvanced(f, store.adv)) : byText);

  // Right-click menu on History rows.
  let menu = $state(null);
  function openMenu(flow, x, y) {
    const ids = store.targetIds;
    const many = ids.length > 1;
    menu = {
      x,
      y,
      items: [
        { label: 'Send to Repeater', hint: '⌘R', run: () => store.sendFlowToRepeater(flow.id) },
        { label: 'Send to Intruder', run: () => store.sendFlowToIntruder(flow.id) },
        { label: 'Send request to Comparer', run: () => store.sendFlowToComparer(flow.id, 'request') },
        { label: 'Send response to Comparer', run: () => store.sendFlowToComparer(flow.id, 'response') },
        'separator',
        { label: 'Copy full URL', run: () => store.copyText(`${flow.scheme}://${flow.host}${flow.path}`, 'URL') },
        { label: 'Copy as cURL', run: () => store.copyCurl(flow.id) },
        { label: 'Copy request body', run: () => store.copyBody(flow.id, 'request') },
        { label: 'Copy response body', run: () => store.copyBody(flow.id, 'response') },
        { label: 'Capture image', run: () => captureFlow(flow.id) },
        'separator',
        {
          label: 'Highlight',
          swatches: HIGHLIGHTS.map((color) => ({
            color,
            on: !many && flow.color === color,
            run: () => store.setColorMany(ids, !many && flow.color === color ? '' : color),
          })),
        },
        { label: 'Clear highlight', disabled: !ids.some((id) => store.flows.find((f) => f.id === id)?.color), run: () => store.setColorMany(ids, '') },
        'separator',
        { label: many ? `Delete ${ids.length} requests` : 'Delete', hint: '⌫', danger: true, run: () => store.deleteFlows(ids) },
      ],
    };
  }

  let current = $derived(store.flows.find((f) => f.id === store.selectedId));

  let capture = $state(null);
  // Highlight-to-decode: the snippet shown in the right-hand mini decoder.
  let miniDecode = $state(null);

  // Close the request/response detail (deselect the current flow).
  function closeDetail() {
    store.selectedId = null;
    store.selectedIds = new Set();
    miniDecode = null;
  }
  async function captureFlow(id) {
    const d = await api.getFlow(id);
    if (!d) return;
    // Mirror what's on screen: if this is the flow currently shown, capture each
    // pane in its chosen view (Pretty / Raw / Hex). For a right-clicked row that
    // isn't open, fall back to the raw text.
    const live = id === store.selectedId;
    const s = d.summary;
    capture = {
      method: s.method,
      host: s.host,
      url: `${s.scheme}://${s.host}${s.path}`,
      status: s.status,
      durationMs: s.durationMs,
      title: s.note || '',
      // Raw text (for the Beautify toggles) + what the UI currently shows and its visible
      // scroll region (for Follow page scroll). Only live when this flow is the open one.
      request: d.requestRaw,
      response: d.responseRaw,
      requestView: (live && reqRef?.displayText?.()) || d.requestRaw,
      responseView: (live && resRef?.displayText?.()) || d.responseRaw,
      reqFrac: live ? reqRef?.visibleFraction?.() ?? null : null,
      resFrac: live ? resRef?.visibleFraction?.() ?? null : null,
    };
  }

  function saveNote(e) {
    if (current && e.currentTarget.value.trim() !== current.note) {
      store.setFlowMeta(current.id, { note: e.currentTarget.value.trim() });
    }
  }

  // A long note is shown trimmed; clicking it opens a modal to read/edit the whole thing.
  const NOTE_TRIM = 50;
  let noteModal = $state(false);
  let noteDraft = $state('');
  function openNoteModal() {
    noteDraft = current?.note ?? '';
    noteModal = true;
  }
  function saveNoteModal() {
    if (current) store.setFlowMeta(current.id, { note: noteDraft.trim() });
    noteModal = false;
  }

  // With several rows selected, a swatch colours all of them.
  function setColor(color) {
    if (store.selectedIds.size > 1) {
      store.setColorMany(store.targetIds, color);
    } else {
      store.setFlowMeta(current.id, { color: current.color === color ? '' : color });
    }
  }

  let detail = $state(null);
  $effect(() => {
    const id = store.selectedId;
    // Refetch when a streaming preview is replaced by the final flow.
    current?.streaming, current?.length;
    if (id == null) return void (detail = null);
    // Show a cached detail immediately (no round-trip, no flicker). On a miss,
    // keep the previous pane visible until the new one loads instead of blanking
    // to a placeholder, which reads as an instant update.
    const cached = store.peekDetail(id);
    if (cached) detail = cached;
    let stale = false;
    store.loadDetail(id).then((d) => !stale && (detail = d));
    return () => (stale = true);
  });

  // Version switcher data (Original / Extension alongside Final), when the flow was
  // edited in Intercept or transformed by an extension. Null hides the switcher.
  function buildVariants(finalRaw, v, side) {
    if (!v || finalRaw == null) return null;
    const orig = side === 'req' ? v.reqOriginal : v.resOriginal;
    const ext = side === 'req' ? v.reqExtension : v.resExtension;
    const list = [{ id: 'final', label: 'Final', raw: finalRaw }];
    // Only show a version alongside Final when it actually differs (i.e. the
    // interceptor or an extension really changed it). An unchanged flow has no
    // switcher at all — it's just the original message.
    if (orig && orig !== finalRaw) list.push({ id: 'original', label: 'Original', raw: orig });
    if (ext && ext !== finalRaw) list.push({ id: 'extension', label: 'Extension', raw: ext });
    return list.length > 1 ? list : null;
  }
  let reqVariants = $derived(buildVariants(detail?.requestRaw, detail?.variants, 'req'));
  let resVariants = $derived(buildVariants(detail?.responseRaw, detail?.variants, 'res'));

  // Draggable split between table and detail panes. The ratio is remembered (like
  // SplitPane) so it survives leaving History and coming back.
  const SPLIT_KEY = brandKey('split.historyDetail');
  function loadSplit() {
    try {
      const v = parseFloat(localStorage.getItem(SPLIT_KEY));
      if (v >= 0.15 && v <= 0.85) return v;
    } catch {
      // storage unavailable; use the default
    }
    return 0.52;
  }
  let split = $state(loadSplit());
  let container;
  let dragging = false;

  function dragStart(e) {
    dragging = true;
    e.currentTarget.setPointerCapture(e.pointerId);
  }
  function dragMove(e) {
    if (!dragging) return;
    const r = container.getBoundingClientRect();
    split = Math.min(0.85, Math.max(0.15, (e.clientY - r.top) / r.height));
  }
  function dragEnd() {
    dragging = false;
    try {
      localStorage.setItem(SPLIT_KEY, String(split));
    } catch {
      // not critical
    }
  }

  // Cmd/Ctrl+R: send the selected request to Repeater (and don't reload the app).
  function onKey(e) {
    if ((e.metaKey || e.ctrlKey) && (e.key === 'r' || e.key === 'R') && store.selectedId != null) {
      e.preventDefault();
      store.sendFlowToRepeater(store.selectedId);
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="history">
  <div class="toolbar">
    <div class="search">
      <svg viewBox="0 0 24 24"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg>
      <input
        class="input"
        bind:value={store.filter}
        oninput={() => store.searchFilter()}
        onkeydown={(e) => e.key === 'Enter' && store.searchBodies()}
        placeholder="Search path, method, status, body… or #15559 for an id"
        spellcheck="false"
      />
    </div>
    <label class="toggle" title={store.scope.length ? 'Show only in-scope hosts' : 'Scope not set (Settings)'}>
      <input type="checkbox" bind:checked={store.scopeOnly} disabled={!store.scope.length} />
      In scope
    </label>
    {#if store.searching}
      <span class="dim searching">Searching body…</span>
    {/if}
    <button class="btn ghost sm" class:on={store.advOpen || advActive(store.adv)} title="Filters: method, status, host, path, include/exclude rules, columns and more" onclick={() => (store.advOpen = !store.advOpen)}>
      Advanced{advActive(store.adv) ? ' •' : ''}
    </button>
    <span class="dim count">{filtered.length} / {store.flows.length}</span>
    {#if store.filtersActive}
      <button class="chip warn" title="Reset every filter (scope, advanced, hide types, search…)" onclick={() => store.clearAllFilters()}>
        Clear filters ×
      </button>
    {/if}
    {#if store.selectedIds.size > 1}
      <button class="chip" title="Clear selection (Esc)" onclick={() => (store.selectedIds = new Set())}>
        {store.selectedIds.size} selected ×
      </button>
    {/if}
    <div class="spacer"></div>
    <button
      class="btn sm browser-btn"
      class:on={store.browser.opened}
      title={!store.browser.exists
        ? 'Set up a browser engine, then launch it wired to the proxy'
        : store.browser.opened
        ? 'A browser is open and wired to the proxy (click to open another window)'
        : 'Launch a browser wired to the proxy so its traffic is recorded'}
      onclick={() => store.openPentestBrowser()}
    >
      🌐 {store.browser.exists ? (store.browser.opened ? 'Browser opened' : 'Open browser') : 'Set up browser'}
    </button>
    <button class="btn ghost sm" onclick={() => store.clear()} disabled={!store.flows.length}>Clear</button>
  </div>

  {#if store.advOpen}
    <div class="adv">
      <!-- Filters: the main include/exclude rule list (host, path, URL, method,
           status, MIME) lives in a modal to keep this panel tidy. -->
      <div class="adv-row">
        <span class="adv-label">Filters</span>
        <button class="btn sm filters-open" class:on={store.advFilterCount > 0} title="Add include or exclude rules across fields" onclick={() => (filtersModal = true)}>
          Include / exclude rules{store.advFilterCount ? ` (${store.advFilterCount})` : ''}
        </button>
        <span class="dim">Match host, path, URL, method, status or MIME and include or exclude it.</span>
      </div>

      <div class="adv-row">
        <span class="adv-label">Method</span>
        <div class="chips">
          {#each METHODS as m}
            <button class="fchip {store.adv.methods.includes(m) ? `on m-${m.toLowerCase()}` : ''}" onclick={() => store.toggleAdv('methods', m)}>{m}</button>
          {/each}
        </div>
      </div>

      <div class="adv-row">
        <span class="adv-label">Status</span>
        <div class="chips">
          {#each STATUS_CLASSES as s}
            <button class="fchip {store.adv.status.includes(s.key) ? `on s-${s.key === 'err' ? 'err' : s.key + 'xx'}` : ''}" onclick={() => store.toggleAdv('status', s.key)}>{s.label}</button>
          {/each}
        </div>
      </div>

      <div class="adv-row wrap">
        <label class="adv-field"><span class="adv-label">Req body</span>
          <select class="input" bind:value={store.adv.reqBody}>
            <option value="any">Any</option>
            <option value="yes">Has body</option>
            <option value="no">No body</option>
          </select>
        </label>
        <label class="adv-field"><span class="adv-label">Resp body</span>
          <select class="input" bind:value={store.adv.resBody}>
            <option value="any">Any</option>
            <option value="yes">Has body</option>
            <option value="no">No body</option>
          </select>
        </label>
        <label class="adv-field"><span class="adv-label">Took ms</span>
          <span class="range">
            <input class="input num" type="number" min="0" placeholder="min" bind:value={store.adv.minMs} />
            <span class="dim">to</span>
            <input class="input num" type="number" min="0" placeholder="max" bind:value={store.adv.maxMs} />
          </span>
        </label>
      </div>

      <div class="adv-row wrap">
        <span class="adv-label">Columns</span>
        <div class="chips">
          {#each HISTORY_COLUMNS as col}
            <button
              class="fchip"
              class:on={store.historyColumns.includes(col.key)}
              disabled={col.always}
              title={col.always ? 'Always shown' : 'Toggle column'}
              onclick={() => store.toggleColumn(col.key)}
            >
              {col.label}
            </button>
          {/each}
        </div>
        <button class="btn ghost sm" onclick={() => store.setHistoryColumns(DEFAULT_COLUMN_KEYS)}>Reset columns</button>
      </div>

      <div class="adv-foot">
        <span class="dim">{filtered.length} / {store.flows.length} shown</span>
        <div class="spacer"></div>
        <button class="btn ghost sm" onclick={() => store.resetAdvanced()} disabled={!advActive(store.adv)}>Clear filters</button>
      </div>
    </div>
  {/if}

  <div class="body">
  <!-- With no request selected the table fills the view; selecting one opens the
       request/response split, and its × closes it fully (table full-height again). -->
  <div class="panes" bind:this={container} style="grid-template-rows: {current ? `${split}fr 5px ${1 - split}fr` : '1fr'}">
    <div class="pane"><FlowTable flows={filtered} oncontext={openMenu} /></div>
    {#if current}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="divider" onpointerdown={dragStart} onpointermove={dragMove} onpointerup={dragEnd}></div>
    <div class="pane detail">
      {#if current}
        <div class="meta">
          <div class="swatches" role="group" aria-label="Highlight">
            {#each HIGHLIGHTS as c}
              <button
                class="swatch c-{c}"
                class:on={current.color === c}
                title={c}
                aria-label="Highlight {c}"
                onclick={() => setColor(c)}
              ></button>
            {/each}
          </div>
          {#if (current.note?.length ?? 0) > NOTE_TRIM}
            <button class="input note note-trim" title="Click to read / edit the full note" onclick={openNoteModal}>
              {current.note.slice(0, NOTE_TRIM)}…
            </button>
          {:else}
            <input
              class="input note"
              placeholder="Add a note for request #{current.id}…"
              value={current.note}
              onchange={saveNote}
              onkeydown={(e) => e.key === 'Enter' && e.currentTarget.blur()}
              spellcheck="false"
            />
          {/if}
          <span class="exact-time mono" title="When this request was captured">{fmtDateTime(current.ts)}</span>
          <button class="close-detail" title="Close request/response" aria-label="Close detail" onclick={closeDetail}>×</button>
        </div>
      {/if}
      <div class="msgs" class:with-decoder={miniDecode}>
      <!-- Right-click anywhere in the request/response panes for the same actions
           as the row menu (Copy URL, Copy as cURL, copy body, Capture, …). -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="msgs-panes" role="presentation"
        oncontextmenu={(e) => { if (current) { e.preventDefault(); openMenu(current, e.clientX, e.clientY); } }}>
      <SplitPane storeKey="history" left={reqPane} right={resPane} />
      {#snippet reqPane()}
        <!-- Detail actions (Copy as cURL, Send to Repeater, Capture image, …) live
             in the row right-click menu, so the pane header stays uncluttered. -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div style="display:contents" onpointerdowncapture={() => (activePane = 'req')} onfocusincapture={() => (activePane = 'req')}>
          <MessageView bind:this={reqRef} title="Request" raw={detail?.requestRaw} placeholder="Select a request to see details"
            truncated={detail?.requestTruncatedView} loadFull={() => store.flowBody(current.id, 'request')} variants={reqVariants}
            isBinary={detail?.requestIsBinary} loadHex={() => store.flowHex(current.id, 'request')}
            ondecode={(t) => (miniDecode = { text: t })} onsend={(t) => store.sendStringToDecoder(t)} />
        </div>
      {/snippet}
      {#snippet resPane()}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div style="display:contents" onpointerdowncapture={() => (activePane = 'res')} onfocusincapture={() => (activePane = 'res')}>
          {#if current?.status === 101}
            <WsView flowId={current.id} handshake={detail?.responseRaw} />
          {:else}
            <MessageView bind:this={resRef} title="Response" raw={detail?.responseRaw} isImage={detail?.responseIsImage} loadImage={() => store.flowImage(current.id)}
              truncated={detail?.responseTruncatedView} loadFull={() => store.flowBody(current.id, 'response')} variants={resVariants}
              isBinary={detail?.responseIsBinary} loadHex={() => store.flowHex(current.id, 'response')}
              ondecode={(t) => (miniDecode = { text: t })} onsend={(t) => store.sendStringToDecoder(t)} />
          {/if}
        </div>
      {/snippet}
      </div>
      {#if miniDecode}
        <aside class="decoder-dock"><MiniDecoder text={miniDecode.text} onclose={() => (miniDecode = null)} /></aside>
      {/if}
      </div>
      {#if current}
        <div class="foot">
          <button class="btn ghost sm" title="Find in the active pane (Cmd/Ctrl+F)" onclick={openFind}>⌕ Find</button>
          <span class="foot-hint">{activePane === 'req' ? 'Request' : 'Response'} · Cmd/Ctrl+F</span>
          <div class="spacer"></div>
          <span class="stat" title="Response time and size">
            {#if current.streaming}streaming…{:else}{current.durationMs} ms · {fmtSize(current.length)}{/if}
          </span>
        </div>
      {/if}
    </div>
    {/if}
  </div>
  </div>
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if capture}
  <CaptureEditor source={capture} onclose={() => (capture = null)} />
{/if}

{#if noteModal && current}
  <Modal title="Note — request #{current.id}" draggable onclose={() => (noteModal = false)}>
    <div class="note-modal">
      <textarea
        class="input mono note-area"
        bind:value={noteDraft}
        placeholder="Add a note for request #{current.id}…"
        spellcheck="false"
      ></textarea>
      <div class="actions">
        <button class="btn primary sm" onclick={saveNoteModal}>Save</button>
        <button class="btn ghost sm" onclick={() => (noteModal = false)}>Cancel</button>
      </div>
    </div>
  </Modal>
{/if}

{#if filtersModal}
  <Modal title="Filters" draggable onclose={() => (filtersModal = false)}>
    <p class="fm-help dim">
      Each rule matches a field by substring, except <b>Extension</b> which matches the URL's real
      file extension exactly (so excluding <code>js</code> hides <code>app.js</code> but not
      <code>data.json</code>). <b>Exclude</b> hides any flow that matches. <b>Include</b> is an
      allowlist: when any include rule is set, a flow must match at least one.
      <b>Aa</b> toggles case sensitivity. Untick a rule to mute it without deleting it, or use
      <b>Enable / Disable all</b> to see everything again and switch back. Rules apply live, and the
      modal can be dragged by its title bar.
    </p>
    {#if !(store.adv.filters ?? []).length}
      <p class="fm-empty dim">No rules yet.</p>
    {:else}
      <div class="fm-rows">
        {#each store.adv.filters as rule, i (i)}
          <div class="fm-row" class:off={rule.enabled === false}>
            <input
              type="checkbox"
              class="fm-on"
              checked={rule.enabled !== false}
              title={rule.enabled === false ? 'Rule muted (click to enable)' : 'Rule active (click to mute)'}
              aria-label="Enable rule"
              onchange={() => store.toggleAdvFilter(i)}
            />
            <button
              type="button"
              class="toggle-btn mode {rule.mode}"
              title="Toggle include / exclude"
              onclick={() => (rule.mode = rule.mode === 'exclude' ? 'include' : 'exclude')}
            >
              {rule.mode === 'exclude' ? 'Exclude' : 'Include'}
            </button>
            <select class="input fm-field" bind:value={rule.field}>
              {#each FILTER_FIELDS as f}<option value={f.key}>{f.label}</option>{/each}
            </select>
            <input
              class="input fm-value"
              placeholder={rule.field === 'ext' ? 'js, png, css…' : 'contains…'}
              bind:value={rule.value}
              spellcheck="false"
            />
            <button
              type="button"
              class="toggle-btn case"
              class:on={rule.sensitive}
              title={rule.sensitive ? 'Case sensitive (click for insensitive)' : 'Case insensitive (click for sensitive)'}
              aria-pressed={rule.sensitive}
              onclick={() => (rule.sensitive = !rule.sensitive)}
            >
              Aa
            </button>
            <button class="fm-del" title="Remove rule" aria-label="Remove rule" onclick={() => store.removeAdvFilter(i)}>×</button>
          </div>
        {/each}
      </div>
    {/if}
    <div class="fm-actions">
      <button class="btn sm" onclick={() => store.addAdvFilter()}>+ Add filter</button>
      <button
        class="btn sm"
        title="Add exclude rules for common junk by file extension: js, css, images, fonts and source maps. Extension matches exactly, so your .json API traffic stays visible."
        onclick={() => store.fillBasicExcludes()}
      >
        Exclude static assets
      </button>
      <button
        class="btn ghost sm"
        title="Mute or unmute every rule at once (keeps them for later)"
        onclick={() => store.setAllAdvFilters(!store.advFiltersAllOn)}
        disabled={!(store.adv.filters ?? []).length}
      >
        {store.advFiltersAllOn ? 'Disable all' : 'Enable all'}
      </button>
      <div class="spacer"></div>
      <button class="btn ghost sm" onclick={() => store.clearAdvFilters()} disabled={!(store.adv.filters ?? []).length}>Clear all</button>
      <button class="btn ghost sm" onclick={() => (filtersModal = false)}>Done</button>
    </div>
  </Modal>
{/if}

<style>
  .history {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .history > .body {
    flex: 1;
  }
  .search {
    position: relative;
    width: min(420px, 50%);
  }
  .search svg {
    position: absolute;
    top: 50%;
    left: 9px;
    width: 14px;
    height: 14px;
    transform: translateY(-50%);
    fill: none;
    stroke: var(--text-faint);
    stroke-width: 2;
    stroke-linecap: round;
  }
  .search .input {
    width: 100%;
    padding-left: 30px;
  }
  .chip {
    height: 24px;
    padding: 0 10px;
    border: 1px solid var(--accent);
    border-radius: 99px;
    background: var(--accent-soft);
    color: var(--accent);
    font: 600 11.5px var(--sans);
    cursor: pointer;
  }
  .chip.warn {
    border-color: var(--amber);
    background: color-mix(in srgb, var(--amber) 16%, transparent);
    color: var(--amber);
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
    font-size: 12px;
    cursor: pointer;
  }
  .toggle input {
    accent-color: var(--accent);
  }
  .count {
    font-size: 12px;
  }
  .searching {
    font-size: 11.5px;
    white-space: nowrap;
  }
  .body {
    display: grid;
    min-height: 0;
  }
  .btn.on {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .adv {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elev);
  }
  .adv-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .adv-row.wrap {
    flex-wrap: wrap;
  }
  .adv-label {
    flex: 0 0 auto;
    min-width: 58px;
    font: 600 10.5px var(--sans);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
  }
  .fchip {
    height: 24px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 600 11px var(--mono);
    cursor: pointer;
  }
  .fchip:hover {
    color: var(--text);
    border-color: var(--accent);
  }
  .fchip.on {
    border-color: currentColor;
    background: color-mix(in srgb, currentColor 14%, transparent);
  }
  .fchip:disabled {
    opacity: 0.55;
    cursor: default;
  }
  .fchip:disabled:hover {
    color: var(--text-dim);
    border-color: var(--border);
  }
  .adv-field {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .adv-field .input {
    height: 28px;
    width: 170px;
  }
  .adv-field .input.num {
    width: 74px;
  }
  .range {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .filters-open {
    height: 28px;
  }
  .adv-foot {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 2px;
    padding-top: 8px;
    border-top: 1px solid var(--border);
    font-size: 12px;
  }
  .panes {
    display: grid;
    min-width: 0;
    min-height: 0;
  }
  .pane {
    min-height: 0;
    overflow: hidden;
  }
  .detail {
    position: relative;
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    gap: 10px;
    padding: 10px;
    background: var(--bg);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 4px 0;
    color: var(--text-dim);
    font: 11.5px var(--mono);
  }
  .foot-hint {
    color: var(--text-faint);
    font: 11px var(--mono);
  }
  .foot .stat {
    color: var(--text-dim);
  }
  .msgs {
    display: grid;
    grid-template-columns: 1fr;
    grid-template-rows: minmax(0, 1fr);
    min-height: 0;
  }
  .msgs.with-decoder {
    /* The dock column sizes to the MiniDecoder's own (draggable) width. */
    grid-template-columns: minmax(0, 1fr) auto;
  }
  .msgs-panes {
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    min-width: 0;
    min-height: 0;
  }
  .decoder-dock {
    min-width: 0;
    min-height: 0;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .swatches {
    display: flex;
    gap: 5px;
  }
  .swatch {
    width: 16px;
    height: 16px;
    padding: 0;
    border: 2px solid transparent;
    border-radius: 50%;
    background: var(--hl);
    cursor: pointer;
    opacity: 0.55;
    transition: opacity 0.12s, transform 0.12s;
  }
  .swatch:hover {
    opacity: 1;
    transform: scale(1.12);
  }
  .swatch.on {
    border-color: var(--text);
    opacity: 1;
  }
  .note {
    flex: 1;
    max-width: 560px;
  }
  /* A long note collapses to a single clickable line (opens the note modal). */
  .note-trim {
    overflow: hidden;
    text-align: left;
    white-space: nowrap;
    text-overflow: ellipsis;
    cursor: pointer;
  }
  .note-trim:hover {
    border-color: var(--accent);
    color: var(--text);
  }
  .note-modal {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: min(620px, 82vw);
  }
  .note-area {
    min-height: 200px;
    resize: vertical;
    line-height: 1.5;
  }
  .note-modal .actions {
    display: flex;
    gap: 8px;
  }
  .exact-time {
    flex-shrink: 0;
    color: var(--text-dim);
    font-size: 11.5px;
    white-space: nowrap;
  }
  .close-detail {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    margin-left: auto;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 7px;
    background: var(--bg-elev);
    color: var(--text-dim);
    font-size: 17px;
    line-height: 1;
    cursor: pointer;
  }
  .close-detail:hover {
    border-color: var(--red);
    color: var(--red);
  }
  .divider {
    border-block: 1px solid var(--border);
    background: var(--bg-elev);
    cursor: row-resize;
  }
  .divider:hover {
    background: var(--accent-soft);
  }

  /* Filters modal */
  .fm-help {
    margin: 0 0 12px;
    font-size: 12px;
    line-height: 1.5;
  }
  .fm-empty {
    margin: 0 0 12px;
    font-size: 12.5px;
  }
  .fm-rows {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
  }
  .fm-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .fm-on {
    flex: 0 0 auto;
    width: 15px;
    height: 15px;
    margin: 0 2px 0 0;
    cursor: pointer;
    accent-color: var(--accent);
  }
  /* A muted rule stays visible and editable but reads as inactive. */
  .fm-row.off .toggle-btn,
  .fm-row.off .input {
    opacity: 0.45;
  }
  /* Clearly-clickable pill toggles; colours adapt to light/dark via the theme
     tokens (--accent/--red/--green defined per theme in app.css). */
  .toggle-btn {
    flex: 0 0 auto;
    height: 26px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-soft);
    color: var(--text);
    font: 600 11px var(--sans);
    cursor: pointer;
    box-shadow: 0 1px 0 rgb(0 0 0 / 0.04);
    transition: filter 0.1s, background 0.1s, border-color 0.1s;
  }
  .toggle-btn:hover {
    filter: brightness(1.08);
    border-color: var(--accent);
  }
  .toggle-btn:active {
    transform: translateY(1px);
  }
  .toggle-btn.mode {
    width: 74px;
    color: #fff;
  }
  .toggle-btn.mode.exclude {
    background: var(--red);
    border-color: var(--red);
  }
  .toggle-btn.mode.include {
    background: var(--green);
    border-color: var(--green);
  }
  .toggle-btn.case {
    width: 40px;
    font-family: var(--mono);
  }
  .toggle-btn.case.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .fm-field {
    flex: 0 0 auto;
    width: 92px;
  }
  .fm-value {
    flex: 1;
    min-width: 0;
  }
  .fm-del {
    flex: 0 0 auto;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text-faint);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
  }
  .fm-del:hover {
    background: var(--bg-hover);
    color: var(--red);
  }
  .fm-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }
  .fm-actions .spacer {
    flex: 1;
  }
</style>
