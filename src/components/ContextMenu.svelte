<script>
  // Right-click menu. `items`: { label, run, danger?, disabled?, swatch? } or 'separator'.
  import { onMount } from 'svelte';

  let { x, y, items, onclose } = $props();
  let menu;
  // How far to pull the menu back so it stays inside the window (measured on mount).
  let overflow = $state({ x: 0, y: 0 });
  let left = $derived(x - overflow.x);
  let top = $derived(y - overflow.y);

  onMount(() => {
    const r = menu.getBoundingClientRect();
    overflow = {
      x: Math.max(0, x + r.width + 6 - window.innerWidth),
      y: Math.max(0, y + r.height + 6 - window.innerHeight),
    };
    menu.focus();
  });

  function pick(item) {
    if (item.disabled) return;
    onclose();
    item.run();
  }
</script>

<svelte:window onmousedown={(e) => !menu?.contains(e.target) && onclose()} onblur={onclose} onresize={onclose} />

<div
  class="menu"
  role="menu"
  tabindex="-1"
  bind:this={menu}
  style="left: {left}px; top: {top}px"
  onkeydown={(e) => e.key === 'Escape' && onclose()}
  oncontextmenu={(e) => e.preventDefault()}
>
  {#each items as item}
    {#if item === 'separator'}
      <div class="sep"></div>
    {:else if item.swatches}
      <div class="swatches" role="group" aria-label={item.label}>
        {#each item.swatches as s}
          <button class="swatch c-{s.color}" class:on={s.on} title={s.color} aria-label="Highlight {s.color}" onclick={() => pick(s)}></button>
        {/each}
      </div>
    {:else}
      <button class="item" class:danger={item.danger} role="menuitem" disabled={item.disabled} onclick={() => pick(item)}>
        {item.label}
        {#if item.hint}<span class="hint">{item.hint}</span>{/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 100;
    min-width: 200px;
    /* Long menus (e.g. the Repeater "+ Header" list) scroll instead of overflowing. */
    max-height: min(70vh, 460px);
    overflow-y: auto;
    overscroll-behavior: contain;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 9px;
    outline: none;
    background: var(--bg-elev);
    box-shadow: 0 10px 30px rgb(0 0 0 / 0.35);
  }
  .item {
    display: flex;
    justify-content: space-between;
    gap: 20px;
    width: 100%;
    height: 28px;
    padding: 0 10px;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text);
    font: 12.5px var(--sans);
    text-align: left;
    align-items: center;
    cursor: pointer;
  }
  .item:hover:not(:disabled) {
    background: var(--accent-soft);
  }
  .item:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .item.danger {
    color: var(--red);
  }
  .hint {
    color: var(--text-faint);
    font-size: 11.5px;
  }
  .sep {
    height: 1px;
    margin: 4px 6px;
    background: var(--border);
  }
  .swatches {
    display: flex;
    gap: 6px;
    padding: 6px 10px;
  }
  .swatch {
    width: 15px;
    height: 15px;
    padding: 0;
    border: 2px solid transparent;
    border-radius: 50%;
    background: var(--hl);
    cursor: pointer;
  }
  .swatch.on,
  .swatch:hover {
    border-color: var(--text);
  }
</style>
