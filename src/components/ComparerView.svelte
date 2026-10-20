<script>
  import { store } from '../lib/store.svelte.js';
  import { lineDiff, diffStats } from '../lib/diff.js';

  let cmp = $derived(store.comparer);
  const byId = (id) => cmp.items.find((it) => it.id === id) ?? null;
  let itemA = $derived(byId(cmp.a));
  let itemB = $derived(byId(cmp.b));

  // Match options for the diff.
  let caseSensitive = $state(true);
  let ignoreWhitespace = $state(false);

  // Which items are being edited inline (typed/pasted directly).
  let editing = $state(new Set());
  function toggleEdit(id) {
    const next = new Set(editing);
    next.has(id) ? next.delete(id) : next.add(id);
    editing = next;
  }
  function addItem() {
    const id = store.newComparerItem();
    editing = new Set([...editing, id]);
  }

  // Diff of the two picked items. Capped so a pathological pair can't lock the UI.
  const ROW_CAP = 6000;
  let ops = $derived(itemA && itemB ? lineDiff(itemA.text, itemB.text, { ignoreCase: !caseSensitive, ignoreWhitespace }) : []);
  let stats = $derived(diffStats(ops));
  let capped = $derived(ops.length > ROW_CAP);
  let shownOps = $derived(capped ? ops.slice(0, ROW_CAP) : ops);

  function pick(slot, id) {
    // Clicking the current pick clears it; picking the other slot's item swaps.
    if (slot === 'a') {
      if (cmp.b === id) cmp.b = cmp.a;
      cmp.a = cmp.a === id ? null : id;
    } else {
      if (cmp.a === id) cmp.a = cmp.b;
      cmp.b = cmp.b === id ? null : id;
    }
  }
  function swap() {
    [cmp.a, cmp.b] = [cmp.b, cmp.a];
  }
  const fmtLen = (t) => `${(t ?? '').length} chars`;
</script>

<div class="comparer">
  <aside class="items">
    <header class="ihead">
      <span class="title">Items <span class="dim">({cmp.items.length})</span></span>
      <div class="spacer"></div>
      <button class="btn sm" title="Add a new item to type or paste into" onclick={addItem}>+ Add</button>
      {#if cmp.items.length}
        <button class="btn ghost sm" onclick={() => store.clearComparer()}>Clear all</button>
      {/if}
    </header>
    {#if !cmp.items.length}
      <p class="empty dim">
        Nothing to compare yet. Click <b>+ Add</b> to paste or type your own text, or in HTTP History
        right-click a request/response and choose <b>Send to Comparer</b>. Then pick one as <b>A</b> and one as <b>B</b>.
      </p>
    {:else}
      <ul class="ilist">
        {#each cmp.items as it (it.id)}
          <li class="item" class:isa={cmp.a === it.id} class:isb={cmp.b === it.id} class:editing={editing.has(it.id)}>
            <div class="irow">
              <div class="pickers">
                <button class="pk" class:on={cmp.a === it.id} title="Compare as A" aria-pressed={cmp.a === it.id} onclick={() => pick('a', it.id)}>A</button>
                <button class="pk" class:on={cmp.b === it.id} title="Compare as B" aria-pressed={cmp.b === it.id} onclick={() => pick('b', it.id)}>B</button>
              </div>
              <div class="imeta">
                {#if editing.has(it.id)}
                  <input class="ilabel-input" bind:value={it.label} placeholder="Label" spellcheck="false" aria-label="Item label" />
                {:else}
                  <span class="ilabel" title={it.label}>{it.label}</span>
                {/if}
                <span class="dim isize">{#if it.kind}<span class="kind">{it.kind}</span>{/if}{fmtLen(it.text)}</span>
              </div>
              <button class="ed" class:on={editing.has(it.id)} title="Edit text" aria-label="Edit item text" aria-pressed={editing.has(it.id)} onclick={() => toggleEdit(it.id)}>
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9" /><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z" /></svg>
              </button>
              <button class="rm" title="Remove" aria-label="Remove item" onclick={() => store.removeComparerItem(it.id)}>×</button>
            </div>
            {#if editing.has(it.id)}
              <textarea class="iedit mono" bind:value={it.text} placeholder="Paste or type the text to compare…" spellcheck="false"></textarea>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
  </aside>

  <section class="diff">
    <header class="dhead">
      <span class="slot a">A</span>
      <span class="dname" title={itemA?.label}>{itemA?.label ?? 'Pick an item as A'}</span>
      <button class="swap" title="Swap A and B" aria-label="Swap A and B" onclick={swap} disabled={!itemA && !itemB}>⇄</button>
      <span class="slot b">B</span>
      <span class="dname" title={itemB?.label}>{itemB?.label ?? 'Pick an item as B'}</span>
      <div class="spacer"></div>
      <label class="opt" title="Match text exactly, including letter case"><input type="checkbox" bind:checked={caseSensitive} /> Case sensitive</label>
      <label class="opt" title="Treat differences in spaces/tabs as equal"><input type="checkbox" bind:checked={ignoreWhitespace} /> Ignore whitespace</label>
      {#if itemA && itemB}
        <span class="stat"><span class="del-c">−{stats.removed}</span> <span class="add-c">+{stats.added}</span></span>
      {/if}
    </header>

    {#if !itemA || !itemB}
      <div class="hint dim">Pick two items (A and B) from the list to see the differences.</div>
    {:else if stats.changed === 0}
      <div class="hint dim">The two items are identical ({stats.same} lines).</div>
    {:else}
      <div class="rows">
        {#each shownOps as op}
          <div class="drow {op.type}">
            <span class="ln">{op.a ?? ''}</span>
            <span class="ln">{op.b ?? ''}</span>
            <span class="sign">{op.type === 'add' ? '+' : op.type === 'del' ? '−' : ''}</span>
            <span class="txt">{op.text}</span>
          </div>
        {/each}
        {#if capped}
          <div class="drow more dim">… diff truncated at {ROW_CAP} lines</div>
        {/if}
      </div>
    {/if}
  </section>
</div>

<style>
  .comparer {
    display: grid;
    grid-template-columns: 280px 1fr;
    height: 100%;
    min-height: 0;
  }
  .items {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--border);
    background: var(--bg-elev);
  }
  .ihead {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-bottom: 1px solid var(--border);
  }
  .title {
    font: 600 12.5px var(--sans);
  }
  .spacer {
    flex: 1;
  }
  .empty {
    margin: 12px 10px;
    font-size: 12px;
    line-height: 1.5;
  }
  .ilist {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 6px;
    overflow: auto;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .item {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
  }
  .irow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .ilabel-input {
    width: 100%;
    height: 22px;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--bg-soft);
    color: var(--text);
    font: 12px var(--mono);
    outline: none;
  }
  .iedit {
    width: 100%;
    min-height: 96px;
    resize: vertical;
    padding: 6px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-soft);
    color: var(--text);
    font: 12px var(--mono);
    white-space: pre;
    outline: none;
  }
  .iedit:focus {
    border-color: var(--accent);
  }
  .ed {
    flex-shrink: 0;
    display: inline-grid;
    place-items: center;
    width: 24px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--bg-soft);
    color: var(--text-dim);
    cursor: pointer;
  }
  .ed:hover {
    color: var(--text);
    border-color: var(--accent);
  }
  .ed.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .item.isa {
    border-color: var(--accent);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .item.isb {
    border-color: var(--green);
    box-shadow: inset 2px 0 0 var(--green);
  }
  .pickers {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .pk {
    width: 22px;
    height: 18px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 700 10px var(--sans);
    cursor: pointer;
  }
  .pk:hover {
    color: var(--text);
    border-color: var(--accent);
  }
  .pk.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .imeta {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    flex: 1;
  }
  .ilabel {
    overflow: hidden;
    font: 12px var(--mono);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .isize {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 10.5px;
  }
  .kind {
    padding: 0 5px;
    border-radius: 4px;
    background: var(--bg-soft);
    color: var(--text-faint);
    font: 700 9px var(--sans);
    text-transform: uppercase;
  }
  .rm {
    width: 22px;
    height: 22px;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-faint);
    font-size: 15px;
    cursor: pointer;
  }
  .rm:hover {
    background: var(--bg-hover);
    color: var(--red);
  }
  .diff {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .dhead {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-bottom: 1px solid var(--border);
  }
  .slot {
    display: inline-grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 5px;
    font: 700 11px var(--sans);
    color: #fff;
  }
  .slot.a {
    background: var(--accent);
  }
  .slot.b {
    background: var(--green);
  }
  .dname {
    max-width: 30%;
    overflow: hidden;
    font: 11.5px var(--mono);
    color: var(--text-dim);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .swap {
    width: 26px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--bg-soft);
    color: var(--text-dim);
    cursor: pointer;
  }
  .swap:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--accent);
  }
  .swap:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--text-dim);
    font-size: 11.5px;
    white-space: nowrap;
    cursor: pointer;
  }
  .opt input {
    accent-color: var(--accent);
  }
  .stat {
    font: 600 12px var(--mono);
  }
  .add-c {
    color: var(--green);
  }
  .del-c {
    color: var(--red);
  }
  .hint {
    display: grid;
    flex: 1;
    place-content: center;
    padding: 24px;
    text-align: center;
    font-size: 13px;
  }
  .rows {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 6px 0;
    font: 12px/1.55 var(--mono);
  }
  .drow {
    display: grid;
    grid-template-columns: 46px 46px 16px 1fr;
    gap: 0;
    padding: 0 8px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .drow .ln {
    text-align: right;
    padding-right: 8px;
    color: var(--text-faint);
    user-select: none;
  }
  .drow .sign {
    text-align: center;
    color: var(--text-faint);
  }
  .drow .txt {
    min-width: 0;
    color: var(--text);
  }
  .drow.add {
    background: color-mix(in srgb, var(--green) 15%, transparent);
  }
  .drow.add .sign {
    color: var(--green);
  }
  .drow.del {
    background: color-mix(in srgb, var(--red) 15%, transparent);
  }
  .drow.del .sign {
    color: var(--red);
  }
  .drow.more {
    display: block;
    padding: 8px;
    text-align: center;
  }
</style>
