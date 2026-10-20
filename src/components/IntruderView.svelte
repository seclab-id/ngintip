<script>
  import CodeEditor from './CodeEditor.svelte';
  import MessageView from './MessageView.svelte';
  import SplitPane from './SplitPane.svelte';
  import { store } from '../lib/store.svelte.js';
  import { fmtSize, statusClass } from '../lib/format.js';
  import { ATTACK_TYPES, MARK, PAYLOAD_TYPES, MAX_REQUESTS, parseTemplate, wrapPosition, clearPositions, defaultPayloadConfig, countPayloads } from '../lib/intruder.js';
  import { tick } from 'svelte';

  const iv = store.intruder;
  let tab = $derived(iv.tabs.find((t) => t.id === iv.activeId) ?? iv.tabs[0]);
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

  const columns = [
    { key: 'n', label: '#' },
    { key: 'status', label: 'Status' },
    { key: 'length', label: 'Length' },
    { key: 'timeMs', label: 'Time' },
  ];

  // Wrap the current selection in the request template as a payload position
  // (Burp-style). With no selection, inserts a §payload§ placeholder at the caret.
  async function addPosition() {
    const ta = document.querySelector('.intruder .req textarea.editor');
    const start = ta ? ta.selectionStart : tab.raw.length;
    const end = ta ? ta.selectionEnd : tab.raw.length;
    const { raw, caret } = wrapPosition(tab.raw, start, end);
    tab.raw = raw;
    if (ta) {
      await tick();
      ta.focus();
      ta.setSelectionRange(caret, caret);
    }
  }
</script>

<div class="intruder">
  <div class="tabs" role="tablist">
    {#each iv.tabs as t (t.id)}
      <div class="tab" class:active={t.id === tab.id} role="tab" tabindex="0" aria-selected={t.id === tab.id} onclick={() => (iv.activeId = t.id)} onkeydown={(e) => e.key === 'Enter' && (iv.activeId = t.id)}>
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
        <div class="panel req">
          <header class="panel-head">
            <span class="panel-title">Request template</span>
            <span class="dim hint">{positions.length} position{positions.length === 1 ? '' : 's'}</span>
            <div class="spacer"></div>
            <button class="btn ghost sm" title="Select text in the request, then click to mark it as a payload position ({MARK}…{MARK})" onclick={addPosition}>Add position {MARK}</button>
            <button class="btn ghost sm" title="Remove all payload positions" onclick={() => (tab.raw = clearPositions(tab.raw))} disabled={!positions.length}>Clear</button>
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
      <div class="results">
        <div class="table">
          <div class="row head">
            {#each columns as c}
              <button class="cell th" onclick={() => sortBy(c.key)}>{c.label}{#if sortKey === c.key}<span class="arrow">{sortDir > 0 ? '↑' : '↓'}</span>{/if}</button>
            {/each}
            <span class="cell th">Payload</span>
          </div>
          <div class="rows">
            {#if !tab.results.length}
              <div class="empty dim">Mark a position, add payloads, then Start attack.</div>
            {/if}
            {#each rows as r (r.n)}
              <button class="row" class:selected={r.n === tab.selected} class:failed={r.error} onclick={() => (tab.selected = r.n)}>
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
          <div class="resp">
            <SplitPane storeKey="intruder-detail" left={reqOut} right={resOut} start={0.5} />
            {#snippet reqOut()}
              <MessageView
                title="Request #{selected.n}"
                raw={selected.requestRaw ?? ''}
                placeholder="Request not kept for this row (large run)" />
            {/snippet}
            {#snippet resOut()}
              <MessageView
                title="Response #{selected.n}"
                raw={selected.error ? '' : selected.responseRaw}
                placeholder={selected.error ? selected.error : 'Response not kept for this row (large run)'} />
            {/snippet}
          </div>
        {/if}
      </div>
    {/snippet}
  </div>
</div>

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
  .results {
    display: grid;
    grid-template-rows: 1fr auto;
    gap: 10px;
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
  .resp {
    display: grid;
    height: 40%;
    min-height: 130px;
  }
  .empty {
    padding: 16px;
    text-align: center;
  }
</style>
