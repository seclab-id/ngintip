<script>
  import CodeEditor from './CodeEditor.svelte';
  import MessageView from './MessageView.svelte';
  import SplitPane from './SplitPane.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import { store } from '../lib/store.svelte.js';
  import { splitMessage } from '../lib/message.js';
  import { toCurl } from '../lib/curl.js';
  import { fmtSize, statusClass } from '../lib/format.js';
  import { ATTACK_TYPES, MARK, PAYLOAD_TYPES, MAX_REQUESTS, parseTemplate, wrapPosition, clearPositions, defaultPayloadConfig, countPayloads } from '../lib/intruder.js';
  import { tick } from 'svelte';
  import { brandKey } from '../lib/brand.js';

  const iv = store.intruder;
  let tab = $derived(iv.tabs.find((t) => t.id === iv.activeId) ?? iv.tabs[0]);

  // Right-click menus: attack tab, request template, result rows and the result's
  // request / response panes (instead of the webview's default Inspect menu).
  let menu = $state(null);

  /** Full URL of a raw request: an absolute request-target, or the tab target + path. */
  function urlOf(raw) {
    const target = String(raw || '').split('\n')[0].split(/\s+/)[1] || '/';
    if (/^https?:\/\//i.test(target)) return target;
    return `${String(tab.target || '').replace(/\/+$/, '')}${target.startsWith('/') ? '' : '/'}${target}`;
  }

  function openTplMenu(e) {
    e.preventDefault();
    const ta = document.querySelector('.intruder .req textarea.editor');
    const hasSel = !!ta && ta.selectionEnd > ta.selectionStart;
    const base = clearPositions(tab.raw);
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: hasSel ? 'Add position on selection' : 'Insert position', hint: `${MARK}…${MARK}`, run: addPosition },
        { label: 'Clear positions', disabled: !positions.length, run: clearPos },
        'separator',
        { label: 'Save to Organizer', hint: store.organizerTargetHint, run: () => store.saveIntruderToOrganizer(tab) },
        { label: 'Send to Repeater', run: () => store.addRepeaterTab({ name: tab.name, target: tab.target, raw: base }) },
        { label: 'Send to Decoder', run: () => store.sendStringToDecoder(base) },
        { label: 'Send to Comparer', run: () => store.addToComparer(base, `${tab.name} template`, 'req') },
        'separator',
        { label: 'Copy URL', run: () => store.copyText(urlOf(base), 'URL') },
        { label: 'Copy as cURL', run: () => store.copyText(toCurl(base, tab.target), 'cURL command') },
        { label: 'Copy raw request', run: () => store.copyText(tab.raw, 'Request') },
      ],
    };
  }

  /** Menu for one attack result (a row, or its request / response pane). */
  function openResultMenu(r, x, y, pane = 'row') {
    tab.selected = r.n;
    const reqRaw = r.requestRaw || '';
    const resRaw = r.error ? '' : r.responseRaw || '';
    const resBody = resRaw ? splitMessage(resRaw).body : '';
    const label = `${tab.name} #${r.n}`;
    const items = [
      { label: 'Save to Organizer', hint: store.organizerTargetHint, run: () => store.saveIntruderResultToOrganizer(tab, r) },
      { label: 'Send request to Repeater', disabled: !reqRaw, run: () => store.addRepeaterTab({ name: label, target: tab.target, raw: reqRaw }) },
      'separator',
    ];
    if (pane !== 'req') {
      items.push(
        { label: 'Copy response body', disabled: !resBody, run: () => store.copyText(resBody, 'Response body') },
        { label: 'Copy raw response', disabled: !resRaw, run: () => store.copyText(resRaw, 'Response') },
        { label: 'Send response to Decoder', disabled: !resRaw, run: () => store.sendStringToDecoder(resRaw) },
        { label: 'Send response to Comparer', disabled: !resRaw, run: () => store.addToComparer(resRaw, `${label} res`, 'res') },
      );
    }
    if (pane !== 'res') {
      if (pane === 'row') items.push('separator');
      items.push(
        { label: 'Copy URL', disabled: !reqRaw, run: () => store.copyText(urlOf(reqRaw), 'URL') },
        { label: 'Copy as cURL', disabled: !reqRaw, run: () => store.copyText(toCurl(reqRaw, tab.target), 'cURL command') },
        { label: 'Copy raw request', disabled: !reqRaw, run: () => store.copyText(reqRaw, 'Request') },
        { label: 'Send request to Comparer', disabled: !reqRaw, run: () => store.addToComparer(reqRaw, `${label} req`, 'req') },
      );
    }
    items.push('separator', { label: 'Copy payload', disabled: !r.payloads?.length, run: () => store.copyText(r.payloads.join(', '), 'Payload') });
    menu = { x, y, items };
  }

  function openTabMenu(t, x, y) {
    iv.activeId = t.id;
    menu = {
      x,
      y,
      items: [
        { label: 'Save to Organizer', hint: store.organizerTargetHint, run: () => store.saveIntruderToOrganizer(t) },
        { label: 'Send to Repeater', run: () => store.addRepeaterTab({ name: t.name, target: t.target, raw: clearPositions(t.raw) }) },
        'separator',
        { label: 'Duplicate', run: () => store.duplicateIntruderTab(t) },
        { label: 'Clear positions', disabled: !parseTemplate(t.raw).positions.length, run: clearPos },
        'separator',
        { label: 'Close tab', danger: true, run: () => store.closeIntruderTab(t.id) },
      ],
    };
  }
  let positions = $derived(parseTemplate(tab.raw).positions);
  // Sniper and battering ram use one shared set; the others use one per position.
  let setCount = $derived(tab.type === 'sniper' || tab.type === 'ram' ? 1 : Math.max(1, positions.length));

  // Normalize the payload sets and keep them long enough for the current attack
  // type. $effect.pre runs before the DOM updates, so the payload cards below always
  // bind to a real config object: never a hole (after switching to a multi-position
  // attack) and never a bare wordlist string (the back-compat shape).
  $effect.pre(() => {
    const cur = tab.sets;
    const base = defaultPayloadConfig();
    let changed = false;
    const complete = (s) =>
      s && typeof s === 'object' && typeof s.type === 'string' && 'list' in s && s.numbers && s.brute && s.nul && s.process;
    const next = cur.map((s) => {
      if (complete(s)) return s;
      changed = true;
      if (s && typeof s === 'object') {
        // Backfill any sub-config an older/partial set is missing, so every field binds.
        return {
          ...base,
          ...s,
          numbers: { ...base.numbers, ...s.numbers },
          brute: { ...base.brute, ...s.brute },
          nul: { ...base.nul, ...s.nul },
          process: { ...base.process, ...s.process },
        };
      }
      return { ...base, type: 'simple', list: typeof s === 'string' ? s : '' };
    });
    while (next.length < setCount) {
      next.push(defaultPayloadConfig());
      changed = true;
    }
    if (changed) tab.sets = next;
  });

  // Live payload count for a set's header, capped for display.
  function countLabel(cfg) {
    const n = countPayloads(cfg);
    const capped = n > MAX_REQUESTS;
    const shown = (capped ? MAX_REQUESTS : n).toLocaleString();
    return `${shown}${capped ? '+' : ''} payload${n === 1 ? '' : 's'}`;
  }
  function procSummary(p) {
    if (!p) return '';
    const bits = [];
    if (p.prefix) bits.push('prefix');
    if (p.suffix) bits.push('suffix');
    if (p.case && p.case !== 'none') bits.push(p.case);
    if (p.encode && p.encode !== 'none') bits.push(p.encode === 'urlall' ? 'url-all' : p.encode);
    if (p.nullByte) bits.push('null byte');
    return bits.join(', ');
  }

  let sortKey = $state('n');
  let sortDir = $state(1);
  const MAX_ROWS = 2000;
  let rows = $derived.by(() => {
    const r = tab.results.toSorted((a, b) => (a[sortKey] > b[sortKey] ? sortDir : a[sortKey] < b[sortKey] ? -sortDir : 0));
    return r.length > MAX_ROWS ? r.slice(0, MAX_ROWS) : r;
  });
  let selected = $derived(tab.results.find((r) => r.n === tab.selected));

  function sortBy(k) {
    if (sortKey === k) sortDir = -sortDir;
    else ((sortKey = k), (sortDir = 1));
  }

  // Arrow keys browse the results in their displayed (sorted) order instead of
  // scrolling the table; Home / End jump to the first / last, PageUp / PageDown by 10.
  let rowsEl = $state();
  const ROW_KEYS = { ArrowDown: 1, ArrowUp: -1, PageDown: 10, PageUp: -10, Home: 0, End: 0 };
  async function onRowsKey(e) {
    if (!(e.key in ROW_KEYS) || !rows.length || e.metaKey || e.ctrlKey || e.altKey) return;
    e.preventDefault();
    const last = rows.length - 1;
    const step = ROW_KEYS[e.key];
    let i = rows.findIndex((r) => r.n === tab.selected);
    if (e.key === 'Home') i = 0;
    else if (e.key === 'End') i = last;
    else if (i < 0) i = step > 0 ? 0 : last;
    else i = Math.max(0, Math.min(last, i + step));
    tab.selected = rows[i].n;
    await tick();
    const btn = rowsEl?.querySelector(`[data-n="${rows[i].n}"]`);
    btn?.focus({ preventScroll: true });
    btn?.scrollIntoView?.({ block: 'nearest' });
  }

  // Results table (top) / result detail (bottom): a draggable divider, remembered.
  const RESULTS_SPLIT_KEY = brandKey('split.intruder-results');
  const STACKED_KEY = brandKey('intruder.detailStacked');
  const RESULTS_SPLIT = 0.42;
  function loadResultsSplit() {
    try {
      const v = parseFloat(localStorage.getItem(RESULTS_SPLIT_KEY));
      if (v >= 0.12 && v <= 0.88) return v;
    } catch {
      // storage unavailable; use the default
    }
    return RESULTS_SPLIT;
  }
  let rsplit = $state(loadResultsSplit());
  let resultsEl = $state();
  let rdrag = false;
  function saveResultsSplit() {
    try {
      localStorage.setItem(RESULTS_SPLIT_KEY, String(rsplit));
    } catch {
      // not critical
    }
  }
  function rDown(e) {
    rdrag = true;
    e.currentTarget.setPointerCapture?.(e.pointerId);
  }
  function rMove(e) {
    if (!rdrag || !resultsEl) return;
    const r = resultsEl.getBoundingClientRect();
    rsplit = Math.min(0.88, Math.max(0.12, (e.clientY - r.top) / r.height));
  }
  function rUp() {
    if (!rdrag) return;
    rdrag = false;
    saveResultsSplit();
  }
  function resetResultsSplit() {
    rsplit = RESULTS_SPLIT;
    saveResultsSplit();
  }

  // Request / response side by side (default) or stacked, remembered.
  let stacked = $state((() => {
    try {
      return localStorage.getItem(STACKED_KEY) === '1';
    } catch {
      return false;
    }
  })());
  function toggleStacked() {
    stacked = !stacked;
    try {
      localStorage.setItem(STACKED_KEY, stacked ? '1' : '0');
    } catch {
      // not critical
    }
  }

  const columns = [
    { key: 'n', label: '#' },
    { key: 'status', label: 'Status' },
    { key: 'length', label: 'Length' },
    { key: 'timeMs', label: 'Time' },
  ];

  // ---- Payload-position undo (Ctrl/Cmd+Z) ----
  // Each "Add position" / "Clear" pushes a snapshot of the template; Ctrl+Z pops it.
  // A manual edit to the template (anything we didn't do) drops the stack, so Ctrl+Z
  // then falls back to the editor's own text undo.
  const posUndo = new Map(); // tab id -> raw snapshots
  let lastOpRaw = null;
  function posStack() {
    let s = posUndo.get(tab.id);
    if (!s) posUndo.set(tab.id, (s = []));
    return s;
  }
  $effect(() => {
    const r = tab.raw;
    if (lastOpRaw !== null && r !== lastOpRaw) {
      posUndo.get(tab.id)?.splice(0);
      lastOpRaw = r;
    }
  });
  function undoPosition() {
    const s = posUndo.get(tab.id);
    if (s?.length) {
      const prev = s.pop();
      tab.raw = prev;
      lastOpRaw = prev;
      return true;
    }
    return false;
  }
  function onReqKeydown(e) {
    if ((e.metaKey || e.ctrlKey) && !e.shiftKey && (e.key === 'z' || e.key === 'Z')) {
      if (undoPosition()) {
        e.preventDefault();
        e.stopPropagation();
      }
    }
  }

  // Wrap the current selection in the request template as a payload position
  // (Burp-style). With no selection, inserts a §payload§ placeholder at the caret.
  async function addPosition() {
    const ta = document.querySelector('.intruder .req textarea.editor');
    const start = ta ? ta.selectionStart : tab.raw.length;
    const end = ta ? ta.selectionEnd : tab.raw.length;
    posStack().push(tab.raw);
    const { raw, caret } = wrapPosition(tab.raw, start, end);
    tab.raw = raw;
    lastOpRaw = raw;
    if (ta) {
      await tick();
      ta.focus();
      ta.setSelectionRange(caret, caret);
    }
  }

  // Remove all positions, but remember the template so Ctrl+Z brings them back.
  function clearPos() {
    posStack().push(tab.raw);
    const r = clearPositions(tab.raw);
    tab.raw = r;
    lastOpRaw = r;
  }
</script>

<div class="intruder">
  <div class="tabs" role="tablist">
    {#each iv.tabs as t (t.id)}
      <div class="tab" class:active={t.id === tab.id} role="tab" tabindex="0" aria-selected={t.id === tab.id} onclick={() => (iv.activeId = t.id)} oncontextmenu={(e) => (e.preventDefault(), openTabMenu(t, e.clientX, e.clientY))} onkeydown={(e) => e.key === 'Enter' && (iv.activeId = t.id)}>
        {#if t.running}<span class="spinner"></span>{/if}
        <span class="name">{t.name}</span>
        <button class="close" aria-label="Close tab" onclick={(e) => (e.stopPropagation(), store.closeIntruderTab(t.id))}>×</button>
      </div>
    {/each}
    <button class="add" title="New attack" aria-label="New attack" onclick={() => store.addIntruderTab()}>+</button>
  </div>

  <div class="toolbar">
    <input class="input mono target" bind:value={tab.target} placeholder="https://example.com" spellcheck="false" />
    <select class="input type" bind:value={tab.type} title={ATTACK_TYPES.find((a) => a.id === tab.type)?.hint}>
      {#each ATTACK_TYPES as a}<option value={a.id}>{a.label}</option>{/each}
    </select>
    <label class="conc">Threads <input class="input mono" type="number" min="1" max="200" bind:value={tab.concurrency} /></label>
    {#if tab.running}
      <button class="btn" onclick={() => store.stopAttack(tab)}>Stop</button>
      <span class="dim">{tab.sent} / {tab.total}</span>
    {:else}
      <button class="btn primary" onclick={() => store.startAttack(tab)}>Start attack</button>
      {#if tab.results.length}<span class="dim">{tab.results.length} sent</span>{/if}
    {/if}
    {#if tab.error}<span class="error">{tab.error}</span>{/if}
  </div>

  <div class="body">
    <SplitPane storeKey="intruder" left={config} right={output} start={0.5} />
    {#snippet config()}
      <div class="config">
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="panel req" onkeydown={onReqKeydown} oncontextmenu={openTplMenu}>
          <header class="panel-head">
            <span class="panel-title">Request template</span>
            <span class="dim hint">{positions.length} position{positions.length === 1 ? '' : 's'}</span>
            <div class="spacer"></div>
            <button class="btn ghost sm" title="Select text in the request, then click to mark it as a payload position ({MARK}…{MARK}). Ctrl/Cmd+Z undoes the last position." onclick={addPosition}>Add position {MARK}</button>
            <button class="btn ghost sm" title="Remove all payload positions (Ctrl/Cmd+Z to undo)" onclick={clearPos} disabled={!positions.length}>Clear</button>
          </header>
          {#key tab.id}
            <CodeEditor bind:value={tab.raw} />
          {/key}
        </div>
        <div class="panel sets">
          <header class="panel-head"><span class="panel-title">Payloads</span><span class="dim hint">pick a type per position</span></header>
          <div class="set-list">
            {#each tab.sets.slice(0, setCount) as cfg, i (i)}
              {#if cfg && typeof cfg === 'object'}
              <div class="set">
                <div class="set-head">
                  <span class="pos-label">{setCount > 1 ? `Position ${i + 1}${positions[i] ? ` (${positions[i]})` : ''}` : 'Payload set'}</span>
                  <div class="spacer"></div>
                  <select class="input sm" bind:value={cfg.type} title={PAYLOAD_TYPES.find((p) => p.id === cfg.type)?.hint}>
                    {#each PAYLOAD_TYPES as p}<option value={p.id}>{p.label}</option>{/each}
                  </select>
                  <span class="count">{countLabel(cfg)}</span>
                </div>

                {#if cfg.type === 'simple'}
                  <textarea class="input mono list" bind:value={cfg.list} spellcheck="false" placeholder="admin&#10;root&#10;1&#10;2"></textarea>
                {:else if cfg.type === 'numbers'}
                  <div class="fields">
                    <label>From<input class="input mono" type="number" bind:value={cfg.numbers.from} /></label>
                    <label>To<input class="input mono" type="number" bind:value={cfg.numbers.to} /></label>
                    <label>Step<input class="input mono" type="number" bind:value={cfg.numbers.step} /></label>
                    <label>Pad<input class="input mono" type="number" min="0" title="Zero-pad to this width" bind:value={cfg.numbers.pad} /></label>
                    <label class="chk"><input type="checkbox" bind:checked={cfg.numbers.hex} />Hex</label>
                  </div>
                {:else if cfg.type === 'brute'}
                  <div class="fields">
                    <label class="grow">Characters<input class="input mono" bind:value={cfg.brute.chars} spellcheck="false" /></label>
                    <label>Min<input class="input mono" type="number" min="1" bind:value={cfg.brute.min} /></label>
                    <label>Max<input class="input mono" type="number" min="1" bind:value={cfg.brute.max} /></label>
                  </div>
                {:else if cfg.type === 'null'}
                  <div class="fields">
                    <label>How many<input class="input mono" type="number" min="0" bind:value={cfg.nul.count} /></label>
                    <span class="dim note">Sends N requests with an empty payload (rate-limit, retry, timing tests).</span>
                  </div>
                {/if}

                <details class="proc">
                  <summary>Processing{procSummary(cfg.process) ? ` · ${procSummary(cfg.process)}` : ''}</summary>
                  <div class="fields">
                    <label class="grow">Prefix<input class="input mono" bind:value={cfg.process.prefix} spellcheck="false" /></label>
                    <label class="grow">Suffix<input class="input mono" bind:value={cfg.process.suffix} spellcheck="false" /></label>
                    <label>Case<select class="input" bind:value={cfg.process.case}><option value="none">None</option><option value="lower">lower</option><option value="upper">UPPER</option></select></label>
                    <label>Encode<select class="input" bind:value={cfg.process.encode}><option value="none">None</option><option value="url">URL-encode</option><option value="urlall">URL-encode all</option><option value="base64">Base64</option></select></label>
                    <label class="chk"><input type="checkbox" bind:checked={cfg.process.nullByte} />Add null byte</label>
                  </div>
                </details>
              </div>
              {/if}
            {/each}
          </div>
        </div>
      </div>
    {/snippet}
    {#snippet output()}
      <!-- The table stays mounted (so the focused row keeps focus for arrow keys); the
           result detail below it gets a draggable divider. -->
      <div
        class="results"
        bind:this={resultsEl}
        style="grid-template-rows: {selected ? `minmax(0, ${rsplit}fr) 8px minmax(0, ${1 - rsplit}fr)` : 'minmax(0, 1fr)'}"
      >
        <div class="table">
          <div class="row head">
            {#each columns as c}
              <button class="cell th" onclick={() => sortBy(c.key)}>{c.label}{#if sortKey === c.key}<span class="arrow">{sortDir > 0 ? '↑' : '↓'}</span>{/if}</button>
            {/each}
            <span class="cell th">Payload</span>
          </div>
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="rows" bind:this={rowsEl} onkeydown={onRowsKey}>
            {#if !tab.results.length}
              <div class="empty dim">Mark a position, add payloads, then Start attack.</div>
            {/if}
            {#each rows as r (r.n)}
              <button class="row" data-n={r.n} class:selected={r.n === tab.selected} class:failed={r.error} onclick={() => (tab.selected = r.n)} oncontextmenu={(e) => (e.preventDefault(), openResultMenu(r, e.clientX, e.clientY, 'row'))}>
                <span class="cell dim mono">{r.n}</span>
                <span class="cell"><span class="badge {statusClass(r.status)}">{r.status || 'ERR'}</span></span>
                <span class="cell dim mono">{fmtSize(r.length)}</span>
                <span class="cell dim mono">{r.timeMs} ms</span>
                <span class="cell mono payload" title={r.payloads.join(', ')}>{r.payloads.join(', ')}</span>
              </button>
            {/each}
          </div>
        </div>
        {#if selected}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="rdivider" title="Drag to resize (double-click to reset)" onpointerdown={rDown} onpointermove={rMove} onpointerup={rUp} ondblclick={resetResultsSplit}></div>
          <div class="resp">
            <div class="resp-bar">
              <span class="resp-title">Result #{selected.n}</span>
              <span class="badge {statusClass(selected.status)}">{selected.status || 'ERR'}</span>
              <span class="dim mono">{fmtSize(selected.length)} · {selected.timeMs} ms</span>
              <span class="mono resp-payload" title={selected.payloads.join(', ')}>{selected.payloads.join(', ')}</span>
              <span class="spacer"></span>
              <span class="dim keys">↑ ↓ to browse</span>
              <button class="btn ghost sm icbtn" title={stacked ? 'Show request and response side by side' : 'Stack the request above the response'} aria-label="Toggle request/response layout" onclick={toggleStacked}>
                {#if stacked}
                  <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M12 4v16" /></svg>
                {:else}
                  <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2" /><path d="M3 12h18" /></svg>
                {/if}
              </button>
              <button class="btn ghost sm icbtn close-res" title="Close the result detail" aria-label="Close result" onclick={() => (tab.selected = null)}>×</button>
            </div>
            {#key stacked}
              <SplitPane vertical={stacked} storeKey={stacked ? 'intruder-detail-stacked' : 'intruder-detail'} left={reqOut} right={resOut} start={0.5} />
            {/key}
            {#snippet reqOut()}
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div style="display:contents" oncontextmenu={(e) => (e.preventDefault(), openResultMenu(selected, e.clientX, e.clientY, 'req'))}>
                <MessageView
                  title="Request #{selected.n}"
                  raw={selected.requestRaw ?? ''}
                  placeholder="Request not kept for this row (large run)" />
              </div>
            {/snippet}
            {#snippet resOut()}
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div style="display:contents" oncontextmenu={(e) => (e.preventDefault(), openResultMenu(selected, e.clientX, e.clientY, 'res'))}>
                <MessageView
                  title="Response #{selected.n}"
                  raw={selected.error ? '' : selected.responseRaw}
                  placeholder={selected.error ? selected.error : 'Response not kept for this row (large run)'} />
              </div>
            {/snippet}
          </div>
        {/if}
      </div>
    {/snippet}
  </div>
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

<style>
  .intruder {
    display: grid;
    grid-template-rows: auto auto 1fr;
    height: 100%;
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
  .tab {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 32px;
    padding: 0 6px 0 12px;
    border: 1px solid transparent;
    border-bottom: 0;
    border-radius: 8px 8px 0 0;
    color: var(--text-dim);
    font-size: 12.5px;
    cursor: default;
  }
  .tab.active {
    border-color: var(--border);
    background: var(--bg);
    color: var(--text);
  }
  .close {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--text-faint);
    font-size: 15px;
    cursor: pointer;
  }
  .close:hover {
    background: var(--bg-soft);
    color: var(--text);
  }
  .add {
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
    width: min(360px, 40%);
  }
  .type {
    cursor: pointer;
  }
  .conc {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
    font-size: 12px;
  }
  .conc input {
    width: 60px;
  }
  .error {
    color: var(--red);
    font-size: 12px;
  }
  /* A grid row with minmax(0,1fr) gives the SplitPane a definite, fillable height
     (like History/Repeater). Without it the split sizes to its content, so the
     request editor + payload cards and the results pane shove each other around. */
  .body {
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    min-height: 0;
    padding: 10px;
  }
  .config {
    display: grid;
    grid-template-rows: 1.2fr 1fr;
    gap: 10px;
    min-height: 0;
  }
  .req,
  .sets {
    min-height: 0;
  }
  .hint {
    margin-left: 8px;
    font-size: 11.5px;
  }
  /* flex:1 + min-height:0 make this the scroll area inside the (overflow:hidden)
     panel, so a tall textarea or many positions scroll here instead of pushing the
     layout around (the resize glitch). */
  .set-list {
    display: flex;
    flex: 1;
    min-height: 0;
    flex-direction: column;
    gap: 10px;
    padding: 10px;
    overflow: auto;
  }
  .set {
    display: flex;
    flex-direction: column;
    gap: 8px;
    flex: 0 0 auto;
    padding: 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-soft);
    font-size: 11.5px;
  }
  .set-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .pos-label {
    color: var(--text-dim);
    font-weight: 600;
  }
  .count {
    color: var(--text-faint);
    font: 11px var(--mono);
    white-space: nowrap;
  }
  .set .list {
    height: 96px;
    min-height: 44px;
    max-height: 340px;
    padding: 8px 10px;
    resize: vertical;
    line-height: 1.5;
  }
  .fields {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 10px;
    align-items: end;
  }
  .fields label {
    display: grid;
    gap: 3px;
    color: var(--text-dim);
  }
  .fields label.grow {
    flex: 1 1 140px;
  }
  .fields input:not([type='checkbox']),
  .fields select {
    width: 84px;
  }
  .fields label.grow input {
    width: 100%;
  }
  .fields .input.sm,
  .set-head .input.sm {
    height: 24px;
  }
  .chk {
    flex-direction: row !important;
    align-items: center;
    gap: 6px !important;
    align-self: end;
    padding-bottom: 4px;
    cursor: pointer;
  }
  .chk input {
    width: auto !important;
  }
  .note {
    flex: 1 1 160px;
    align-self: center;
    line-height: 1.4;
  }
  .proc {
    border-top: 1px solid var(--border);
    padding-top: 8px;
  }
  .proc summary {
    color: var(--text-dim);
    cursor: pointer;
    user-select: none;
  }
  .proc summary:hover {
    color: var(--text);
  }
  .proc .fields {
    margin-top: 8px;
  }
  /* Rows are set inline: the table alone, or table / divider / detail. */
  .results {
    display: grid;
    min-height: 0;
    margin-left: 10px;
  }
  .table {
    display: grid;
    grid-template-rows: auto 1fr;
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    overflow: hidden;
    background: var(--bg-elev);
    font-size: 12.5px;
  }
  .rows {
    overflow: auto;
  }
  .table .row {
    display: grid;
    grid-template-columns: 56px 64px 76px 72px 1fr;
    align-items: center;
    width: 100%;
    height: 26px;
    padding: 0 8px;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: none;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .table .row.head {
    background: var(--bg-soft);
    cursor: default;
  }
  .table .row:hover:not(.head) {
    background: var(--bg-hover);
  }
  .table .row.selected {
    background: var(--accent-soft);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .table .row.failed .payload {
    color: var(--red);
  }
  .cell {
    overflow: hidden;
    padding: 0 6px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .th {
    border: 0;
    background: none;
    color: var(--text-dim);
    font: inherit;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    text-align: left;
    cursor: pointer;
  }
  .arrow {
    color: var(--accent);
  }
  .rdivider {
    display: grid;
    place-items: center;
    cursor: row-resize;
    touch-action: none;
  }
  .rdivider::after {
    content: '';
    width: 44px;
    height: 3px;
    border-radius: 2px;
    background: var(--border);
  }
  .rdivider:hover::after {
    background: var(--accent);
  }
  /* The result detail fills its area: a header bar, then request / response. */
  .resp {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr);
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
    overflow: hidden;
  }
  .resp-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 4px 6px 4px 10px;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
  }
  .resp-title {
    flex: none;
    font-weight: 600;
  }
  .resp-payload {
    min-width: 0;
    overflow: hidden;
    color: var(--text-dim);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .resp-bar .spacer {
    flex: 1;
  }
  .resp-bar .keys {
    flex: none;
    font-size: 11px;
  }
  .resp-bar .icbtn {
    flex: none;
  }
  .close-res {
    font-size: 16px;
    line-height: 1;
  }
  .empty {
    padding: 16px;
    text-align: center;
  }
</style>
