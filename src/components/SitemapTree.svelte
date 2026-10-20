<script>
  // Host → path tree of the rows History currently shows; picking a node filters the table.
  import { buildSitemap, filterSitemap, allKeys, flatten } from '../lib/sitemap.js';

  let { flows, selected = $bindable(null), oncontext } = $props();

  let tree = $derived(buildSitemap(flows));
  let expanded = $state(new Set());

  // Search: filter the tree to matching hosts/paths and auto-expand so every match
  // is visible; with no query the normal expand/collapse state applies.
  let query = $state('');
  let searching = $derived(!!query.trim());
  let filtered = $derived(searching ? filterSitemap(tree, query) : tree);
  let rows = $derived(flatten(filtered, searching ? allKeys(filtered) : expanded));

  function toggle(node) {
    const next = new Set(expanded);
    next.has(node.key) ? next.delete(node.key) : next.add(node.key);
    expanded = next;
  }

  function pick(node) {
    selected = selected === node.key ? null : node.key;
  }

  // While searching the tree is fully expanded, so the caret reflects that.
  const isOpen = (node) => searching || expanded.has(node.key);
</script>

<div class="sitemap">
  <div class="search">
    <svg class="sicon" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="11" cy="11" r="7" /><line x1="21" y1="21" x2="16.65" y2="16.65" /></svg>
    <input class="sinput mono" type="text" placeholder="Search hosts / paths…" bind:value={query} spellcheck="false" aria-label="Search sitemap" />
    {#if searching}
      <button class="sclear" title="Clear search" aria-label="Clear search" onclick={() => (query = '')}>×</button>
    {/if}
  </div>
  <div class="tree" role="tree" aria-label="Sitemap">
  {#if !rows.length}
    <p class="empty">{searching ? 'No matches.' : 'No hosts yet.'}</p>
  {/if}
  {#each rows as { node, depth } (node.key)}
    <div class="node" class:selected={selected === node.key} style="padding-left: {8 + depth * 14}px" role="treeitem" aria-selected={selected === node.key}>
      {#if node.children.length}
        <button class="caret" aria-label={isOpen(node) ? 'Collapse' : 'Expand'} onclick={() => toggle(node)}>
          {isOpen(node) ? '▾' : '▸'}
        </button>
      {:else}
        <span class="caret"></span>
      {/if}
      <button
        class="name"
        class:host={depth === 0}
        title={node.key}
        onclick={() => pick(node)}
        ondblclick={() => toggle(node)}
        oncontextmenu={(e) => (e.preventDefault(), oncontext?.(node, e.clientX, e.clientY))}
      >
        {depth === 0 ? node.name : `/${node.name}`}
      </button>
      <span class="count">{node.count}</span>
    </div>
  {/each}
  </div>
</div>

<style>
  .sitemap {
    display: flex;
    flex-direction: column;
    min-height: 0;
    /* Fill the parent when it has a definite height; otherwise size to content
       (e.g. inside Settings' max-height sitemap box, which scrolls itself). */
    height: 100%;
    max-height: 100%;
  }
  .search {
    display: flex;
    flex-shrink: 0;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--border);
    /* Stay pinned at the top while the tree (or the surrounding box) scrolls. */
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--bg);
  }
  .sicon {
    flex-shrink: 0;
    color: var(--text-faint);
  }
  .sinput {
    flex: 1;
    min-width: 0;
    height: 24px;
    padding: 0 4px;
    border: 0;
    background: none;
    color: var(--text);
    font-size: 12px;
    outline: none;
  }
  .sinput::placeholder {
    color: var(--text-faint);
  }
  .sclear {
    flex-shrink: 0;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--text-faint);
    font-size: 14px;
    line-height: 1;
    cursor: pointer;
  }
  .sclear:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .tree {
    flex: 1;
    min-height: 0;
    padding: 4px 0;
    overflow: auto;
    font-size: 12.5px;
  }
  .node {
    display: flex;
    align-items: center;
    gap: 2px;
    height: 24px;
    padding-right: 8px;
  }
  .node:hover {
    background: var(--bg-hover);
  }
  .node.selected {
    background: var(--accent-soft);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .caret {
    display: inline-grid;
    flex-shrink: 0;
    place-items: center;
    width: 16px;
    height: 16px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text-dim);
    font-size: 10px;
    cursor: pointer;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text);
    font: 12px var(--mono);
    text-align: left;
    white-space: nowrap;
    text-overflow: ellipsis;
    cursor: pointer;
  }
  .name.host {
    font-family: var(--sans);
    font-weight: 600;
  }
  .count {
    color: var(--text-faint);
    font: 11px var(--mono);
  }
  .empty {
    margin: 12px;
    color: var(--text-faint);
  }
</style>
