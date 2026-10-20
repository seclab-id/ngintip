<script>
  import { untrack } from 'svelte';
  import { store } from '../lib/store.svelte.js';
  import { fmtSize, fmtDateTime, statusClass, methodClass } from '../lib/format.js';
  import { HISTORY_COLUMNS, queryOf } from '../lib/columns.js';

  // `oncontext(flow, x, y)`: right-click on a row (already selected by then).
  let { flows, oncontext } = $props();

  function onContext(e, f) {
    e.preventDefault();
    const inSelection = store.selectedIds.size ? store.selectedIds.has(f.id) : store.selectedId === f.id;
    if (!inSelection) store.selectRow(f.id);
    oncontext?.(f, e.clientX, e.clientY);
  }

  const ROW = 28;
  const OVERSCAN = 10;
  // Only the columns enabled for this project, in catalog order.
  let columns = $derived(HISTORY_COLUMNS.filter((c) => store.historyColumns.includes(c.key)));
  let template = $derived(columns.map((c) => c.width).join(' '));

  // Sort value for a column (derived columns don't map to a flow field directly).
  function sortVal(f, key) {
    if (key === 'query') return queryOf(f.path);
    if (key === 'resBody') return f.length > 0 ? 1 : 0;
    if (key === 'reqBody') return f.reqLength > 0 ? 1 : 0;
    if (key === 'modified') return f.modified ? 1 : 0;
    return f[key];
  }

  // Sort is persisted per project in the store, so re-opening keeps the order.
  let sortKey = $derived(store.historySort.key);
  let sortDir = $derived(store.historySort.dir);
  let viewport = $state();
  let scrollTop = $state(0);
  let height = $state(0);
  let follow = $state(true);

  let rows = $derived.by(() => {
    const k = sortKey;
    const d = sortDir;
    return flows.toSorted((a, b) => {
      const av = sortVal(a, k);
      const bv = sortVal(b, k);
      return av > bv ? d : av < bv ? -d : 0;
    });
  });
  let start = $derived(Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN));
  let end = $derived(Math.min(rows.length, Math.ceil((scrollTop + height) / ROW) + OVERSCAN));
  let visible = $derived(rows.slice(start, end));

  // Where the newest rows land depends on the sort: when sorting by #/Time
  // descending, new flows appear at the TOP, otherwise at the bottom. The tail
  // follow must stick to that end (bug: it always scrolled to the bottom, so
  // sorting newest-first jumped the view to the oldest rows on every new flow).
  let newestAtTop = $derived((sortKey === 'id' || sortKey === 'ts') && sortDir < 0);

  // Stick to the newest end while new traffic arrives, like a tail -f.
  $effect(() => {
    rows.length;
    const atTop = newestAtTop;
    if (untrack(() => follow) && viewport) {
      viewport.scrollTop = atTop ? 0 : viewport.scrollHeight;
      scrollTop = viewport.scrollTop; // keep the virtual window in sync
    }
  });

  function onScroll() {
    scrollTop = viewport.scrollTop;
    follow = newestAtTop
      ? viewport.scrollTop <= ROW
      : viewport.scrollTop + viewport.clientHeight >= viewport.scrollHeight - ROW;
  }

  // Warm the detail cache for the rows around the selection so arrow-key
  // navigation and the next click open instantly. `rows` is read untracked so
  // this only fires when the selection moves, not on every incoming flow.
  $effect(() => {
    const id = store.selectedId;
    if (id == null) return;
    untrack(() => {
      const i = rows.findIndex((f) => f.id === id);
      if (i === -1) return;
      for (const n of [rows[i + 1], rows[i - 1], rows[i + 2], rows[i - 2]]) {
        if (n) store.prefetchDetail(n.id);
      }
    });
  });

  // Prefetch a row's detail once the pointer rests on it briefly, so a click
  // that follows opens with no wait. The short delay avoids firing for every row
  // during a fast mouse sweep; prefetchDetail de-dupes anything already loaded.
  let hoverTimer;
  function onEnter(f) {
    clearTimeout(hoverTimer);
    hoverTimer = setTimeout(() => store.prefetchDetail(f.id), 100);
  }
  function onLeave() {
    clearTimeout(hoverTimer);
  }

  function sortBy(key) {
    store.setHistorySort(key);
  }

  // Double-click the "#" cell to copy that flow's id (handy for referencing it, e.g.
  // searching "#15559"). Stop propagation so the dblclick doesn't also toggle the row.
  async function copyId(e, id) {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(String(id));
      store.notify(`Copied flow id ${id}`);
    } catch {
      /* clipboard unavailable */
    }
  }

  function onKey(e) {
    if ((e.key === 'Delete' || e.key === 'Backspace') && store.targetIds.length) {
      e.preventDefault();
      store.deleteFlows(store.targetIds);
      return;
    }
    if (e.key === 'a' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      store.selectedIds = new Set(rows.map((f) => f.id));
      return;
    }
    if (e.key === 'Escape') {
      store.selectedIds = new Set();
      return;
    }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const i = rows.findIndex((f) => f.id === store.selectedId);
    const next = rows[Math.min(rows.length - 1, Math.max(0, i + (e.key === 'ArrowDown' ? 1 : -1)))];
    if (!next) return;
    store.selectRow(next.id);
    const idx = rows.indexOf(next);
    const top = idx * ROW;
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (top + ROW > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = top + ROW - viewport.clientHeight;
  }
</script>

<div class="table">
  <div class="row head" style="grid-template-columns: {template}">
    {#each columns as col}
      <button class="cell th" onclick={() => sortBy(col.key)}>
        {col.label}
        {#if sortKey === col.key}<span class="arrow">{sortDir > 0 ? '↑' : '↓'}</span>{/if}
      </button>
    {/each}
  </div>

  <div class="viewport" role="grid" aria-label="HTTP history" bind:this={viewport} bind:clientHeight={height} onscroll={onScroll} onkeydown={onKey} tabindex="0">
    {#if rows.length === 0}
      {#if store.flows.length}
        <div class="empty">
          <p>No requests match the current filters.</p>
          <p class="dim">{store.flows.length} request{store.flows.length === 1 ? '' : 's'} hidden.</p>
          <p><button class="btn sm" onclick={() => store.clearAllFilters()}>Clear all filters</button></p>
        </div>
      {:else}
        <div class="empty">
          <p>No traffic yet.</p>
          <p class="dim">Point your browser at the proxy <span class="mono">{store.proxy.host}:{store.proxy.port}</span></p>
        </div>
      {/if}
    {/if}
    <div style="height: {rows.length * ROW}px; position: relative">
      <div style="transform: translateY({start * ROW}px)">
        {#each visible as f (f.id)}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <div
            class="row {f.color ? `c-${f.color}` : ''}"
            style="grid-template-columns: {template}"
            class:marked={f.color}
            class:selected={store.selectedIds.size ? store.selectedIds.has(f.id) : store.selectedId === f.id}
            class:failed={f.error}
            onclick={(e) => store.selectRow(f.id, { toggle: e.metaKey || e.ctrlKey, range: e.shiftKey }, rows)}
            oncontextmenu={(e) => onContext(e, f)}
            onpointerenter={() => onEnter(f)}
            onpointerleave={onLeave}
          >
            {#each columns as col (col.key)}
              {#if col.key === 'id'}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <span class="cell dim mono idcell" title="Double-click to copy id" ondblclick={(e) => copyId(e, f.id)}>{f.id}</span>
              {:else if col.key === 'ts'}
                <span class="cell dim mono" title={fmtDateTime(f.ts)}>{fmtDateTime(f.ts)}</span>
              {:else if col.key === 'method'}
                <span class="cell"><span class="method {methodClass(f.method)}">{f.method}</span></span>
              {:else if col.key === 'host'}
                <span class="cell host">{#if f.scheme === 'https'}<span class="lock">●</span>{/if}{f.host}</span>
              {:else if col.key === 'path'}
                <span class="cell mono path" title={f.path}>{f.path}</span>
              {:else if col.key === 'query'}
                <span class="cell mono path dim" title={queryOf(f.path)}>{queryOf(f.path)}</span>
              {:else if col.key === 'status'}
                <span class="cell"><span class="badge {statusClass(f.status)}">{f.status || 'ERR'}</span></span>
              {:else if col.key === 'length'}
                <span class="cell dim mono">{f.streaming ? '…' : fmtSize(f.length)}</span>
              {:else if col.key === 'reqLength'}
                <span class="cell dim mono">{f.reqLength ? fmtSize(f.reqLength) : ''}</span>
              {:else if col.key === 'resBody'}
                <span class="cell mono flag" class:yes={f.length > 0}>{f.streaming ? '…' : f.length > 0 ? '✓' : '·'}</span>
              {:else if col.key === 'reqBody'}
                <span class="cell mono flag" class:yes={f.reqLength > 0}>{f.reqLength > 0 ? '✓' : '·'}</span>
              {:else if col.key === 'mime'}
                <span class="cell dim">{f.mime}</span>
              {:else if col.key === 'durationMs'}
                <span class="cell dim mono" title={f.streaming ? 'Still streaming' : fmtDateTime(f.ts)}>{f.streaming ? 'streaming' : `${f.durationMs} ms`}</span>
              {:else if col.key === 'modified'}
                <span class="cell mono flag" class:yes={f.modified} title={f.modified ? 'Edited in the interceptor or transformed by an extension' : 'Unchanged'}>{f.modified ? '✓' : '·'}</span>
              {:else if col.key === 'note'}
                <span class="cell note" title={f.note}>{f.note}</span>
              {/if}
            {/each}
          </div>
        {/each}
      </div>
    </div>
  </div>
</div>

<style>
  .table {
    display: grid;
    grid-template-rows: auto 1fr;
    height: 100%;
    min-height: 0;
    font-size: 12.5px;
  }
  .row {
    display: grid;
    align-items: center;
    height: 28px;
    padding: 0 8px;
    cursor: default;
  }
  .flag {
    color: var(--text-faint);
    text-align: center;
  }
  .flag.yes {
    color: var(--green);
  }
  .row:not(.head):nth-child(even) {
    background: var(--row-alt);
  }
  .row:not(.head):hover {
    background: var(--bg-hover);
  }
  .row.marked,
  .row.marked:nth-child(even) {
    background: color-mix(in srgb, var(--hl) 16%, transparent);
    box-shadow: inset 3px 0 0 var(--hl);
  }
  .row.marked:hover {
    background: color-mix(in srgb, var(--hl) 24%, transparent);
  }
  .note {
    color: var(--amber);
  }
  .row.selected,
  .row.selected:hover,
  .row.selected:nth-child(even) {
    background: var(--accent-soft);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .row.failed .path {
    color: var(--red);
  }
  .head {
    border-bottom: 1px solid var(--border);
    background: var(--bg-elev);
  }
  .cell {
    overflow: hidden;
    padding: 0 6px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .idcell {
    cursor: copy;
  }
  .th {
    border: 0;
    background: none;
    color: var(--text-dim);
    font: inherit;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-align: left;
    text-transform: uppercase;
    cursor: pointer;
  }
  .th:hover {
    color: var(--text);
  }
  .arrow {
    color: var(--accent);
  }
  .viewport {
    position: relative;
    overflow: auto;
    outline: none;
  }
  .lock {
    margin-right: 6px;
    color: var(--green);
    font-size: 7px;
    vertical-align: middle;
  }
  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    gap: 4px;
    text-align: center;
    color: var(--text-dim);
  }
  .empty p {
    margin: 0;
  }
</style>
