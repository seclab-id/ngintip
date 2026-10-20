<script>
  import { tick } from 'svelte';
  import CodeEditor from './CodeEditor.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import Modal from './Modal.svelte';
  import FindBar from './FindBar.svelte';
  import { matchRanges, markHtmlRanges } from '../lib/find.js';
  import { store } from '../lib/store.svelte.js';
  import { methodClass, statusClass, fmtDateTime } from '../lib/format.js';
  import { prettyBody, splitMessage } from '../lib/message.js';
  import { toCurl } from '../lib/curl.js';
  import { headerMenuItems, bodyMenuItems } from '../lib/editormenu.js';
  import { highlightHttp } from '../lib/highlight.js';
  import { softWrapLong } from '../lib/message.js';
  import { redirectInfo } from '../lib/redirect.js';
  import { INTERCEPT_FILTER_FIELDS } from '../lib/interceptfilter.js';

  const it = store.intercept;
  let filterOpen = $state(false);
  let filterCount = $derived((it.filter.rules ?? []).filter((r) => String(r.value).trim() !== '').length);
  const applyFilter = () => store.applyInterceptFilter();
  function toggleRuleMode(i) {
    const r = it.filter.rules[i];
    r.mode = r.mode === 'include' ? 'exclude' : 'include';
    applyFilter();
  }
  function toggleRuleCase(i) {
    it.filter.rules[i].sensitive = !it.filter.rules[i].sensitive;
    applyFilter();
  }
  // WebSocket intercept shares this view: held WS data messages appear in the same
  // queue, and selecting one swaps the detail panel to a simple payload editor.
  const wsIt = store.wsIntercept;
  const OPCODES = { 0: 'continuation', 1: 'text', 2: 'binary', 8: 'close', 9: 'ping', 10: 'pong' };
  let selKind = $state('http'); // which queue the toolbar/detail/keys act on
  let wsSelected = $derived(wsIt.queue.find((q) => q.key === wsIt.selectedKey));
  let wsEdited = $derived(!!(wsSelected && !wsSelected.binary && wsIt.draft !== wsSelected.text));
  // Unified, chronological queue of HTTP + WS held messages.
  let allHeld = $derived(
    [...it.queue.map((q) => ({ ...q, kind: 'http' })), ...wsIt.queue.map((q) => ({ ...q, kind: 'ws' }))].sort(
      (a, b) => (a.ts ?? 0) - (b.ts ?? 0),
    ),
  );
  let anyHeld = $derived(it.queue.length + wsIt.queue.length > 0);
  let anyEnabled = $derived(it.enabled || wsIt.enabled);
  function selectItem(item) {
    selKind = item.kind;
    if (item.kind === 'ws') store.selectWsHeld(item.key);
    else store.selectHeld(item.key);
  }
  function forwardCurrent() {
    if (selKind === 'ws') store.forwardWsHeld();
    else store.forwardHeld();
  }
  function dropCurrent() {
    if (selKind === 'ws') store.dropWsHeld();
    else store.dropHeld();
  }

  let selected = $derived(it.queue.find((q) => q.key === it.selectedKey));
  let isRequest = $derived(selected?.phase !== 'response');
  // A held response that is a redirect can be followed into Repeater.
  let canFollow = $derived(!!(selected && !isRequest && redirectInfo(selected.raw)));
  let edited = $derived(selected && it.draft !== selected.raw);
  let beautified = $derived(selected ? prettyBody(it.draft) : null);
  // Compare the editable draft against what arrived.
  let tab = $state('edited');
  let menu = $state(null); // right-click menu on a queued item

  // In-pane find. The Edited tab is a CodeEditor (it searches itself); the Original
  // tab is a read-only <pre>, so it gets its own find state here.
  let editedRef = $state();
  let origText = $derived(softWrapLong(selected?.original ?? selected?.raw ?? ''));
  let origFindOpen = $state(false);
  let origQuery = $state('');
  let origOpts = $state({ caseSensitive: false, regex: false, word: false });
  let origIdx = $state(0);
  let origPre = $state();
  let origHovered = $state(false);
  // Debounce the query/options so typing stays smooth on a large held message.
  let origDeb = $state({ q: '', opts: { caseSensitive: false, regex: false, word: false } });
  $effect(() => {
    const q = origQuery;
    const o = { caseSensitive: origOpts.caseSensitive, regex: origOpts.regex, word: origOpts.word };
    if ((origText?.length ?? 0) <= 20000) { origDeb = { q, opts: o }; return; }
    const t = setTimeout(() => (origDeb = { q, opts: o }), 110);
    return () => clearTimeout(t);
  });
  // Find layers its <mark>s on top of the syntax-highlighted HTML so the Original
  // view keeps its colours while searching.
  let origMatches = $derived(origFindOpen && origDeb.q ? matchRanges(origText, origDeb.q, origDeb.opts) : null);
  let origBody = $derived(origMatches?.count ? markHtmlRanges(highlightHttp(origText), origMatches.ranges, origIdx) : highlightHttp(origText));

  $effect(() => {
    const count = origMatches?.count ?? 0;
    if (count && origIdx >= count) origIdx = 0;
    if (!count) return;
    origIdx;
    tick().then(() => origPre?.querySelector('mark.mk.on')?.scrollIntoView?.({ block: 'center' }));
  });
  function origStep(d) {
    const count = origMatches?.count ?? 0;
    if (count) origIdx = (origIdx + d + count) % count;
  }
  function openFind() {
    if (tab === 'original') {
      origFindOpen = true;
      origIdx = 0;
    } else {
      editedRef?.openFind?.();
    }
  }
  function closeOrigFind() {
    origFindOpen = false;
    origQuery = '';
  }
  // Cmd/Ctrl+F over the Original pane opens its find (the Edited CodeEditor handles
  // its own). Reset the Original search when a different message is selected.
  $effect(() => {
    it.selectedKey;
    origFindOpen = false;
    origQuery = '';
  });
  $effect(() => {
    const handler = (e) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'f' || e.key === 'F') && tab === 'original') {
        if (origHovered || origPre?.contains(document.activeElement)) {
          e.preventDefault();
          openFind();
        }
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  });

  // Same "+ Header" / "+ Body" dropdowns as Repeater, editing the held draft.
  const draftGet = () => it.draft;
  const draftSet = (v) => (it.draft = v);
  function openHeaderMenu(e) {
    const r = e.currentTarget.getBoundingClientRect();
    menu = { x: r.left, y: r.bottom + 4, items: headerMenuItems(draftGet, draftSet) };
  }
  function openBodyMenu(e) {
    const r = e.currentTarget.getBoundingClientRect();
    menu = { x: r.left, y: r.bottom + 4, items: bodyMenuItems(draftGet, draftSet) };
  }

  // Send the held request (with any edits) to Repeater.
  function sendToRepeater() {
    if (!selected || !isRequest) return;
    store.addRepeaterTab({ name: selected.host, target: `${selected.scheme}://${selected.host}`, raw: it.draft });
  }

  // The "send elsewhere / copy" actions for a held message. Shared by the queue item
  // right-click and the request/response detail right-click. A held response has no
  // request to replay, so it only offers the response-side actions.
  function heldMenuItems(item, raw) {
    const origin = `${item.scheme}://${item.host}`;
    const url = `${item.scheme}://${item.host}${item.path ?? ''}`;
    const body = splitMessage(raw).body;
    if (item.phase !== 'response') {
      return [
        { label: 'Send to Repeater', hint: '⌘R', run: () => store.addRepeaterTab({ name: item.host, target: origin, raw }) },
        { label: 'Send to Intruder', run: () => store.sendToIntruder({ target: origin, raw }) },
        { label: 'Send to Comparer', run: () => store.addToComparer(raw, `${item.host} req`, 'req') },
        { label: 'Send to Decoder', run: () => store.sendStringToDecoder(raw) },
        'separator',
        { label: 'Copy URL', run: () => store.copyText(url, 'URL') },
        { label: 'Copy as cURL', run: () => store.copyText(toCurl(raw, origin), 'cURL command') },
        { label: 'Copy request body', disabled: !body, run: () => store.copyText(body, 'Request body') },
        { label: 'Copy raw request', run: () => store.copyText(raw, 'Request') },
      ];
    }
    return [
      { label: 'Send to Comparer', run: () => store.addToComparer(raw, `${item.host} res`, 'res') },
      { label: 'Send to Decoder', run: () => store.sendStringToDecoder(raw) },
      'separator',
      { label: 'Copy URL', run: () => store.copyText(url, 'URL') },
      { label: 'Copy response body', disabled: !body, run: () => store.copyText(body, 'Response body') },
      { label: 'Copy raw response', run: () => store.copyText(raw, 'Response') },
    ];
  }

  // Right-click a queued item: select it first (but never the open one, so in-progress
  // edits survive), then show the actions for it.
  function openItemMenu(e, item) {
    e.preventDefault();
    if (item.key !== it.selectedKey) store.selectHeld(item.key);
    const raw = item.key === it.selectedKey ? it.draft : item.raw;
    menu = { x: e.clientX, y: e.clientY, items: heldMenuItems(item, raw || '') };
  }

  // Right-click the request/response detail: same actions, on the edited draft.
  function openDetailMenu(e) {
    if (!selected) return;
    e.preventDefault();
    menu = { x: e.clientX, y: e.clientY, items: heldMenuItems(selected, it.draft || selected.raw || '') };
  }

  function onKey(e) {
    if (!(e.metaKey || e.ctrlKey)) return;
    const active = selKind === 'ws' ? wsSelected : selected;
    if (!active) return;
    if (e.key === 'Enter') {
      e.preventDefault();
      if (selKind === 'ws') store.forwardWsHeld();
      else store.forwardHeld({ holdResponse: e.shiftKey });
    } else if (e.key === 'Backspace' && e.shiftKey) {
      e.preventDefault();
      dropCurrent();
    } else if ((e.key === 'r' || e.key === 'R') && selKind === 'http' && isRequest) {
      e.preventDefault();
      sendToRepeater();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="intercept">
  <div class="toolbar">
    <button class="btn primary" onclick={forwardCurrent} disabled={!selected && !wsSelected}>
      Forward <kbd>⌘↵</kbd>
    </button>
    <button
      class="btn"
      title="Send this request, then also hold its response"
      onclick={() => store.forwardHeld({ holdResponse: true })}
      disabled={selKind !== 'http' || !selected || !isRequest}
    >
      Forward + response <kbd class="plain">⇧⌘↵</kbd>
    </button>
    <button class="btn" onclick={dropCurrent} disabled={!selected && !wsSelected}>
      Drop <kbd class="plain">⇧⌘⌫</kbd>
    </button>
    {#if selKind === 'http' && edited}
      <span class="edited">edited</span>
      <button class="btn ghost sm" onclick={() => (it.draft = selected.raw)}>Reset</button>
    {:else if selKind === 'ws' && wsEdited}
      <span class="edited">edited</span>
      <button class="btn ghost sm" onclick={() => (wsIt.draft = wsSelected.text)}>Reset</button>
    {/if}
    <div class="spacer"></div>
    <label class="toggle" title="Hold every in-scope response, not only ones requested per request">
      <input type="checkbox" checked={it.responses} onchange={() => store.toggleInterceptResponses()} />
      Intercept responses
    </label>
    <label class="toggle" title="Hold WebSocket data messages for in-scope connections opened while this is on">
      <input type="checkbox" checked={wsIt.enabled} onchange={() => store.toggleWsIntercept()} />
      WebSocket
    </label>
    <button class="btn ghost sm" class:on={filterCount > 0} title="Advanced filter: only hold requests/responses that match (saved with the project)" onclick={() => (filterOpen = true)}>
      Filter{filterCount ? ` (${filterCount})` : ''}
    </button>
    <button class="btn ghost sm" disabled={selKind !== 'http' || !selected || !isRequest} title="Send to Repeater (⌘R)" onclick={sendToRepeater}>
      Send to Repeater
    </button>
    {#if canFollow}
      <button class="btn ghost sm" title="Open the redirect target (Location) in Repeater" onclick={() => store.followHeldRedirect(selected)}>
        Follow redirect →
      </button>
    {/if}
    <button class="btn ghost sm" onclick={() => { store.forwardAllHeld(); store.forwardAllWsHeld(); }} disabled={!anyHeld}>
      Forward all ({it.queue.length + wsIt.queue.length})
    </button>
  </div>

  {#if !anyEnabled && !anyHeld}
    <div class="empty">
      <p>Intercept is off.</p>
      <p class="dim">Turn it on so requests (and responses, when asked) are held before being forwarded. Use the WebSocket toggle above to hold WebSocket messages too.</p>
      <button class="btn primary" onclick={() => store.toggleIntercept()}>Turn intercept on</button>
    </div>
  {:else if !anyHeld}
    <div class="empty">
      <p>Waiting for traffic…</p>
      <p class="dim">
        Held here: requests{it.responses ? ', responses' : ''}{wsIt.enabled ? ', WebSocket messages' : ''}.
      </p>
    </div>
  {:else}
    <div class="grid">
      <div class="panel queue">
        <header class="panel-head"><span class="panel-title">Queue</span></header>
        <div class="list">
          {#each allHeld as item (item.kind + ':' + item.key)}
            {#if item.kind === 'ws'}
              <button class="item" class:selected={selKind === 'ws' && item.key === wsIt.selectedKey} onclick={() => selectItem(item)}>
                <span class="phase ws">WS</span>
                <span class="wsdir" title={item.direction === 'client' ? 'Client to server' : 'Server to client'}>{item.direction === 'client' ? '↑' : '↓'}</span>
                <span class="where">
                  <span class="host">{item.host}</span>
                  <span class="path mono">{item.text}</span>
                </span>
              </button>
            {:else}
              <button class="item" class:selected={selKind === 'http' && item.key === it.selectedKey} onclick={() => selectItem(item)} oncontextmenu={(e) => openItemMenu(e, item)}>
                {#if item.phase === 'response'}
                  <span class="phase res">RES</span>
                  <span class="badge {statusClass(item.status)}">{item.status}</span>
                {:else}
                  <span class="phase">REQ</span>
                  <span class="method {methodClass(item.method)}">{item.method}</span>
                {/if}
                <span class="where">
                  <span class="host">{item.host}</span>
                  <span class="path mono">{item.path}</span>
                </span>
              </button>
            {/if}
          {/each}
        </div>
      </div>
      {#if selKind === 'ws' && wsSelected}
        <div class="panel detail ws">
          <header class="panel-head">
            <span class="tag ws">WEBSOCKET</span>
            <span class="wsdir-label mono">{wsSelected.direction === 'client' ? 'Client → server' : 'Server → client'} · {OPCODES[wsSelected.opcode] ?? `opcode ${wsSelected.opcode}`}</span>
            <span class="dim target mono">{wsSelected.host}</span>
            {#if wsSelected.ts}<span class="held-time mono">{fmtDateTime(wsSelected.ts)}</span>{/if}
            <div class="spacer"></div>
          </header>
          {#if wsSelected.binary}
            <pre class="code orig ws-bin">{wsIt.draft}</pre>
            <div class="foot"><span class="foot-hint">Binary payload - forwarded or dropped as-is (not editable as text).</span></div>
          {:else}
            {#key wsIt.selectedKey}
              <textarea class="ws-editor mono" bind:value={wsIt.draft} spellcheck="false"></textarea>
            {/key}
            <div class="foot"><span class="foot-hint">Edit the payload, then Forward (⌘↵) or Drop (⇧⌘⌫).</span></div>
          {/if}
        </div>
      {:else}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="panel detail" class:req={isRequest} class:res={!isRequest} oncontextmenu={openDetailMenu}>
        <header class="panel-head">
          <span class="tag {isRequest ? 'req' : 'res'}">{isRequest ? 'REQUEST' : 'RESPONSE'}</span>
          <div class="modes" role="tablist">
            <button class="mode" class:active={tab === 'edited'} role="tab" aria-selected={tab === 'edited'} onclick={() => (tab = 'edited')}>
              Edited{edited ? ' •' : ''}
            </button>
            <button class="mode" class:active={tab === 'original'} role="tab" aria-selected={tab === 'original'} onclick={() => (tab = 'original')}>
              Original
            </button>
            {#if selected?.original}
              <span class="ext-tag" title="An interceptor-scope extension transformed this message; Original shows what arrived on the wire">ext</span>
            {/if}
          </div>
          {#if selected}
            <span class="dim target mono">
              {isRequest ? '' : `${selected.method} `}{selected.scheme}://{selected.host}{isRequest ? '' : selected.path}
            </span>
            {#if selected.ts}<span class="held-time mono" title="When this message was intercepted">{fmtDateTime(selected.ts)}</span>{/if}
          {/if}
          <div class="spacer"></div>
          {#if tab === 'edited'}
            <button class="btn ghost sm" title="Add a header (static, random User-Agent, spoof IP, host tricks, Content-Length…)" onclick={openHeaderMenu}>+ Header ▾</button>
            <button class="btn ghost sm" title="Insert a body skeleton (JSON, form, XML…) with Content-Type and Content-Length" onclick={openBodyMenu}>+ Body ▾</button>
            <button
              class="btn ghost sm"
              title="Format body JSON / XML / HTML"
              disabled={!beautified || beautified === it.draft}
              onclick={() => (it.draft = beautified)}
            >
              Beautify
            </button>
          {/if}
        </header>
        {#if tab === 'original'}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <pre class="code orig" bind:this={origPre} onpointerenter={() => (origHovered = true)} onpointerleave={() => (origHovered = false)}>{@html origBody}</pre>
          {#if origFindOpen}
            <FindBar bind:value={origQuery} bind:opts={origOpts} error={origMatches?.error} count={origMatches?.count ?? 0} index={origIdx} oninput={() => (origIdx = 0)} onstep={origStep} onclose={closeOrigFind} />
          {/if}
        {:else}
          {#key it.selectedKey}
            <CodeEditor bind:this={editedRef} bind:value={it.draft} />
          {/key}
        {/if}
        <div class="foot">
          <button class="btn ghost sm" title="Find in this message (Cmd/Ctrl+F)" onclick={openFind}>⌕ Find</button>
          <span class="foot-hint">Cmd/Ctrl+F</span>
        </div>
      </div>
      {/if}
    </div>
  {/if}

  {#if menu}
    <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
  {/if}

  {#if filterOpen}
    <Modal title="Intercept filter" draggable onclose={() => (filterOpen = false)}>
      <div class="filter-body">
        <p class="dim fnote">Only hold messages that match. An exclude always wins; with no rules, everything in scope is held. Saved with the project.</p>
        {#each it.filter.rules as rule, i (i)}
          <div class="frule">
            <button class="toggle-btn mode {rule.mode}" onclick={() => toggleRuleMode(i)}>{rule.mode === 'include' ? 'Include' : 'Exclude'}</button>
            <select class="input field" bind:value={rule.field} onchange={applyFilter}>
              {#each INTERCEPT_FILTER_FIELDS as f}<option value={f.id}>{f.label}</option>{/each}
            </select>
            <input class="input mono val" placeholder="contains…" spellcheck="false" bind:value={rule.value} onchange={applyFilter} />
            <button class="toggle-btn case" class:on={rule.sensitive} title="Case sensitive" onclick={() => toggleRuleCase(i)}>Aa</button>
            <button class="rm" title="Remove rule" aria-label="Remove rule" onclick={() => store.removeInterceptFilterRule(i)}>×</button>
          </div>
        {/each}
        <div class="frule-actions">
          <button class="btn ghost sm" onclick={() => store.addInterceptFilterRule()}>+ Add rule</button>
          {#if it.filter.rules.length}<button class="btn ghost sm" onclick={() => store.clearInterceptFilter()}>Clear all</button>{/if}
        </div>
      </div>
    </Modal>
  {/if}
</div>

<style>
  .intercept {
    display: grid;
    grid-template-rows: auto 1fr;
    height: 100%;
  }
  .modes {
    display: flex;
    gap: 2px;
    margin-left: 10px;
    padding: 2px;
    border-radius: 7px;
    background: var(--bg-soft);
  }
  .mode {
    height: 22px;
    padding: 0 9px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font: 500 11.5px var(--sans);
    cursor: pointer;
  }
  .mode.active {
    background: var(--bg-elev);
    color: var(--text);
  }
  .ext-tag {
    margin-left: 8px;
    align-self: center;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--accent);
    color: #fff;
    font: 600 9.5px var(--sans);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .orig {
    flex: 1;
    margin: 0;
    overflow: auto;
  }
  .orig :global(mark.mk) {
    padding: 0;
    border-radius: 2px;
    background: color-mix(in srgb, var(--amber) 40%, transparent);
    color: inherit;
  }
  .orig :global(mark.mk.on) {
    background: var(--amber);
    color: #1b1300;
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 8px;
    border-top: 1px solid var(--border);
    background: var(--bg-elev);
  }
  .foot-hint {
    color: var(--text-faint);
    font: 11px var(--mono);
  }
  .edited {
    padding: 2px 8px;
    border-radius: 99px;
    background: color-mix(in srgb, var(--amber) 16%, transparent);
    color: var(--amber);
    font-size: 11.5px;
    font-weight: 600;
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
  .phase {
    flex-shrink: 0;
    padding: 1px 5px;
    border-radius: 4px;
    background: color-mix(in srgb, var(--amber) 18%, transparent);
    color: var(--amber);
    font: 700 9.5px var(--mono);
    letter-spacing: 0.04em;
  }
  .phase.res {
    background: color-mix(in srgb, var(--blue) 16%, transparent);
    color: var(--blue);
  }
  /* Big, theme-aware phase label so it's obvious in light AND dark which side is
     being intercepted (request vs response). */
  .tag {
    padding: 2px 10px;
    border-radius: 99px;
    font: 700 10.5px var(--mono);
    letter-spacing: 0.07em;
  }
  .tag.req {
    background: color-mix(in srgb, var(--amber) 20%, transparent);
    color: var(--amber);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--amber) 45%, transparent);
  }
  .tag.res {
    background: color-mix(in srgb, var(--blue) 20%, transparent);
    color: var(--blue);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--blue) 45%, transparent);
  }
  .phase.ws {
    background: color-mix(in srgb, var(--cyan) 18%, transparent);
    color: var(--cyan);
  }
  .tag.ws {
    background: color-mix(in srgb, var(--cyan) 20%, transparent);
    color: var(--cyan);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--cyan) 45%, transparent);
  }
  .wsdir {
    flex-shrink: 0;
    color: var(--cyan);
    font: 700 11px var(--mono);
  }
  .wsdir-label {
    font-size: 11px;
    color: var(--text-dim);
  }
  .ws-editor {
    flex: 1;
    width: 100%;
    box-sizing: border-box;
    border: 0;
    resize: none;
    padding: 10px 12px;
    background: var(--bg-elev);
    color: var(--text);
    font: 12px/1.5 var(--mono);
    outline: none;
  }
  .ws-bin {
    flex: 1;
    overflow: auto;
    margin: 0;
  }
  /* Tint the detail panel's top edge to match the phase. */
  .detail.req {
    box-shadow: inset 0 2px 0 color-mix(in srgb, var(--amber) 55%, transparent);
  }
  .detail.res {
    box-shadow: inset 0 2px 0 color-mix(in srgb, var(--blue) 55%, transparent);
  }
  .item .badge {
    min-width: 0;
  }
  kbd.plain {
    background: var(--bg-hover);
    color: var(--text-dim);
  }
  .grid {
    display: grid;
    grid-template-columns: minmax(220px, 300px) 1fr;
    gap: 10px;
    min-height: 0;
    padding: 10px;
  }
  .list {
    overflow: auto;
  }
  .item {
    display: flex;
    gap: 8px;
    align-items: center;
    width: 100%;
    padding: 8px 12px;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: none;
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }
  .item:hover {
    background: var(--bg-hover);
  }
  .item.selected {
    background: var(--accent-soft);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .where {
    display: grid;
    min-width: 0;
  }
  .host,
  .path {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .path {
    color: var(--text-dim);
    font-size: 11.5px;
  }
  .target {
    margin-left: 8px;
    font-size: 11.5px;
  }
  .held-time {
    margin-left: 10px;
    flex-shrink: 0;
    color: var(--text-faint);
    font-size: 11.5px;
    white-space: nowrap;
  }
  .empty {
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 6px;
    color: var(--text-dim);
    text-align: center;
  }
  .empty p {
    margin: 0;
  }
  .empty .btn {
    margin-top: 10px;
  }
  /* Highlight the toolbar Filter button when rules are active. */
  .btn.ghost.sm.on {
    border-color: var(--accent);
    color: var(--accent);
  }

  /* ---- Advanced filter modal ---- */
  .filter-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: min(560px, 82vw);
  }
  .fnote {
    margin: 0 0 4px;
    font-size: 12px;
    line-height: 1.5;
  }
  .frule {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .frule .field {
    flex: 0 0 auto;
    width: 96px;
  }
  .frule .val {
    flex: 1;
    min-width: 0;
  }
  .frule-actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .rm {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    flex: 0 0 auto;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: none;
    color: var(--text-faint);
    font-size: 16px;
    cursor: pointer;
  }
  .rm:hover {
    border-color: var(--red);
    color: var(--red);
  }
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
    transition: filter 0.1s, background 0.1s, border-color 0.1s;
  }
  .toggle-btn:hover {
    filter: brightness(1.08);
    border-color: var(--accent);
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
</style>
