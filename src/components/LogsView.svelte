<script>
  // Process log: proxy errors (TLS handshake / pinning), upstream failures,
  // and info/debug about what the proxy is doing.
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte.js';
  import { fmtDateTime } from '../lib/format.js';
  import Modal from './Modal.svelte';

  const LEVELS = ['all', 'error', 'warn', 'info', 'debug'];
  // Show only the most recent page by default, so opening Logs with thousands of
  // lines stays instant; "Retrieve all" renders the rest on demand.
  const PAGE = 200;
  let level = $state('all');
  let filter = $state('');
  let showAll = $state(false);
  let viewport;
  let selected = $state(new Set());

  function toggle(id) {
    const next = new Set(selected);
    next.has(id) ? next.delete(id) : next.add(id);
    selected = next;
  }
  let allShownSelected = $derived(visible.length > 0 && visible.every((l) => selected.has(l.id)));
  function toggleAll() {
    selected = allShownSelected ? new Set() : new Set(visible.map((l) => l.id));
  }
  async function deleteSelected() {
    await store.deleteLogs([...selected]);
    selected = new Set();
  }
  async function deleteOne(id) {
    await store.deleteLogs([id]);
    if (selected.has(id)) toggle(id);
  }

  // Detail modal for a single log line (full message, selectable).
  let detail = $state(null); // log entry | null
  const closeDetail = () => (detail = null);
  async function deleteFromModal(id) {
    await deleteOne(id);
    closeDetail();
  }

  onMount(() => store.seenLogs());
  // Clear the unread badge whenever the log is on screen.
  $effect(() => {
    store.logs.length;
    store.seenLogs();
  });

  let rank = { error: 3, warn: 2, info: 1, debug: 0 };
  // Matching lines, newest first (store.logs is oldest -> newest).
  let shown = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    const out = store.logs.filter(
      (l) =>
        (level === 'all' || l.level === level) &&
        (!q || l.message.toLowerCase().includes(q) || l.source.toLowerCase().includes(q)),
    );
    out.reverse();
    return out;
  });
  // Only the latest page is rendered until the user retrieves everything.
  let visible = $derived(showAll ? shown : shown.slice(0, PAGE));
  let hidden = $derived(shown.length - visible.length);
  let counts = $derived.by(() => {
    const c = { error: 0, warn: 0, info: 0, debug: 0 };
    for (const l of store.logs) c[l.level] = (c[l.level] ?? 0) + 1;
    return c;
  });

  // Newest is at the top; when a new line arrives and the user is already at the
  // top, keep them pinned to it (otherwise leave their scroll position alone).
  $effect(() => {
    shown.length;
    if (viewport && viewport.scrollTop <= 8) viewport.scrollTop = 0;
  });
</script>

<div class="logs">
  <div class="toolbar">
    <div class="levels" role="tablist">
      {#each LEVELS as l}
        <button class="lvl" class:active={level === l} onclick={() => (level = l)}>
          {l}{l !== 'all' && counts[l] ? ` ${counts[l]}` : ''}
        </button>
      {/each}
    </div>
    <input class="input filter" bind:value={filter} placeholder="Filter log…" spellcheck="false" />
    <div class="spacer"></div>
    <span class="dim count">{showAll ? shown.length : `latest ${visible.length}`} / {store.logs.length}</span>
    {#if hidden > 0}
      <button class="btn ghost sm" title="Render every matching log line (may be slow for very large logs)" onclick={() => (showAll = true)}>Retrieve all ({hidden} more)</button>
    {/if}
    <label class="follow" title="Select all shown">
      <input type="checkbox" checked={allShownSelected} onchange={toggleAll} disabled={!visible.length} /> Select
    </label>
    {#if selected.size}
      <button class="btn ghost sm" onclick={deleteSelected}>Delete ({selected.size})</button>
    {/if}
    <button class="btn ghost sm" onclick={() => store.clearLogs()} disabled={!store.logs.length}>Clear all</button>
  </div>

  <div class="rows" bind:this={viewport}>
    {#if !shown.length}
      <div class="empty dim">{store.logs.length ? 'Nothing matches the filter.' : 'No log entries yet. Proxy errors and activity show up here.'}</div>
    {/if}
    {#each visible as l (l.id)}
      <div class="row l-{l.level}" class:sel={selected.has(l.id)} role="button" tabindex="0" title="Show details"
        onclick={() => (detail = l)}
        onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), (detail = l))}>
        <input class="rowcheck" type="checkbox" checked={selected.has(l.id)} onclick={(e) => e.stopPropagation()} onchange={() => toggle(l.id)} aria-label="Select log line" />
        <span class="ts mono" title={fmtDateTime(l.ts)}>{fmtDateTime(l.ts)}</span>
        <span class="badge lvl-{l.level}">{l.level}</span>
        <span class="src mono">{l.source}</span>
        <span class="msg">{l.message}</span>
        <button class="del" title="Delete this line" aria-label="Delete log line" onclick={(e) => { e.stopPropagation(); deleteOne(l.id); }}>×</button>
      </div>
    {/each}
  </div>

  {#if detail}
    <Modal title="Log entry" onclose={closeDetail}>
      <div class="dgrid">
        <span class="dk">Time</span><span class="dv mono">{fmtDateTime(detail.ts)}</span>
        <span class="dk">Level</span><span class="dv"><span class="badge lvl-{detail.level}">{detail.level}</span></span>
        <span class="dk">Source</span><span class="dv mono">{detail.source}</span>
      </div>
      <pre class="dmsg mono">{detail.message}</pre>
      <div class="dactions">
        <button class="btn ghost sm" onclick={() => store.copyText(detail.message, 'Log message')}>Copy message</button>
        <button class="btn ghost sm danger" onclick={() => deleteFromModal(detail.id)}>Delete</button>
        <button class="btn sm" onclick={closeDetail}>Close</button>
      </div>
    </Modal>
  {/if}
</div>

<style>
  .logs {
    display: grid;
    grid-template-rows: auto 1fr;
    height: 100%;
  }
  .levels {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 7px;
    background: var(--bg-soft);
  }
  .lvl {
    height: 24px;
    padding: 0 10px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font: 500 12px var(--sans);
    text-transform: capitalize;
    cursor: pointer;
  }
  .lvl.active {
    background: var(--bg-elev);
    color: var(--text);
  }
  .filter {
    width: min(280px, 30%);
  }
  .follow {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
    font-size: 12px;
    cursor: pointer;
  }
  .follow input {
    accent-color: var(--accent);
  }
  .count {
    font-size: 12px;
  }
  .rows {
    overflow: auto;
    padding: 4px 0;
    font-size: 12.5px;
  }
  .row {
    display: grid;
    grid-template-columns: 20px 150px 58px 74px 1fr 22px;
    gap: 10px;
    align-items: baseline;
    padding: 3px 12px;
  }
  .row:nth-child(even) {
    background: var(--row-alt);
  }
  .row {
    cursor: pointer;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.sel {
    background: var(--accent-soft);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .rowcheck {
    align-self: center;
    accent-color: var(--accent);
    opacity: 0;
  }
  .row:hover .rowcheck,
  .row.sel .rowcheck {
    opacity: 1;
  }
  .del {
    align-self: center;
    width: 20px;
    height: 20px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--text-faint);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
  }
  .row:hover .del {
    opacity: 1;
  }
  .del:hover {
    background: var(--bg-hover);
    color: var(--red);
  }
  .ts {
    color: var(--text-faint);
    font-size: 11.5px;
  }
  .src {
    color: var(--text-dim);
    font-size: 11.5px;
  }
  .msg {
    white-space: pre-wrap;
    word-break: break-word;
  }
  .badge {
    justify-self: start;
    min-width: 0;
    padding: 0 7px;
    font-size: 10.5px;
    text-transform: uppercase;
  }
  .lvl-error {
    background: color-mix(in srgb, var(--red) 16%, transparent);
    color: var(--red);
  }
  .lvl-warn {
    background: color-mix(in srgb, var(--amber) 16%, transparent);
    color: var(--amber);
  }
  .lvl-info {
    background: color-mix(in srgb, var(--blue) 16%, transparent);
    color: var(--blue);
  }
  .lvl-debug {
    background: var(--bg-soft);
    color: var(--text-dim);
  }
  .l-error .msg {
    color: var(--text);
  }
  .empty {
    padding: 24px;
    text-align: center;
  }

  /* Log detail modal */
  .dgrid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 12px;
    align-items: center;
    margin-bottom: 12px;
    font-size: 12.5px;
  }
  .dk {
    color: var(--text-faint);
  }
  .dv {
    overflow-wrap: anywhere;
  }
  .dmsg {
    margin: 0;
    padding: 10px 12px;
    max-height: 320px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-soft);
    color: var(--text);
    font-size: 12.5px;
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .dactions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
  .btn.danger:not(:disabled) {
    color: var(--red);
  }
  .btn.danger:not(:disabled):hover {
    border-color: var(--red);
    background: color-mix(in srgb, var(--red) 12%, transparent);
  }
</style>
