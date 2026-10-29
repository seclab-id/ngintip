<script>
  // Right-click menu. `items`: { label, run, danger?, disabled?, hint? }, a highlight row
  // { label, swatches: [{ color | hex, on, run, label?, title? }] } (`color` is a named
  // highlight, `hex` any CSS colour), a nested menu { label, submenu: [items] } (opens on
  // hover, click, Enter or ArrowRight), or 'separator'.
  import { onMount, tick } from 'svelte';

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

  // ---- Nested menu (one level) ----
  // `sub` is the open submenu: its parent's index, items and element. Hovering another
  // item closes it after a short grace period, so a diagonal move from the parent into
  // the submenu does not snap it shut.
  let sub = $state(null); // { index, items, el }
  let subEl = $state();
  let subPos = $state({ left: 0, top: 0 });
  let closeTimer;

  function openSub(index, item, el) {
    clearTimeout(closeTimer);
    if (item.disabled || !item.submenu?.length || sub?.index === index) return;
    const r = el.getBoundingClientRect();
    sub = { index, items: item.submenu, el };
    subPos = { left: r.right - 2, top: r.top - 5 };
  }
  function closeSubSoon() {
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => (sub = null), 200);
  }
  function keepSub() {
    clearTimeout(closeTimer);
  }
  // Keyboard: open the submenu and focus its first item; ArrowLeft goes back.
  async function enterSub(index, item, el) {
    openSub(index, item, el);
    await tick();
    subEl?.querySelector('button:not(:disabled)')?.focus();
  }
  function leaveSub() {
    const parent = sub?.el;
    sub = null;
    parent?.focus();
  }
  // A new menu (same component, new items) starts with no submenu open.
  $effect(() => {
    void items;
    sub = null;
  });
  // Keep the submenu inside the window: flip it to the menu's left, shift it up.
  $effect(() => {
    if (!sub || !subEl) return;
    const r = sub.el.getBoundingClientRect();
    const w = subEl.offsetWidth;
    const h = subEl.offsetHeight;
    let l = r.right - 2;
    if (l + w + 6 > window.innerWidth) l = Math.max(6, r.left - w + 2);
    let t = r.top - 5;
    if (t + h + 6 > window.innerHeight) t = Math.max(6, window.innerHeight - h - 6);
    subPos = { left: l, top: t };
  });
</script>

<svelte:window onmousedown={(e) => !menu?.contains(e.target) && onclose()} onblur={onclose} onresize={onclose} />

<div
  class="menu"
  role="menu"
  tabindex="-1"
  bind:this={menu}
  style="left: {left}px; top: {top}px"
  onkeydown={(e) => {
    // Escape closes only the menu, not a dialog or editor underneath that also listens for it.
    if (e.key === 'Escape') {
      e.stopPropagation();
      onclose();
    }
  }}
  oncontextmenu={(e) => e.preventDefault()}
  onscroll={() => (sub = null)}
>
  {#each items as item, i}
    {#if item === 'separator'}
      <div class="sep"></div>
    {:else if item.swatches}
      <div class="swatches" role="group" aria-label={item.label} onpointerenter={closeSubSoon}>
        {#each item.swatches as s}
          <button
            class="swatch {s.color ? `c-${s.color}` : ''}"
            class:on={s.on}
            style={s.hex ? `--hl: ${s.hex}` : undefined}
            title={s.title ?? s.color}
            aria-label={s.label ?? `Highlight ${s.color}`}
            onclick={() => pick(s)}
          ></button>
        {/each}
      </div>
    {:else if item.submenu}
      <button
        class="item parent"
        class:open={sub?.index === i}
        role="menuitem"
        aria-haspopup="menu"
        aria-expanded={sub?.index === i}
        disabled={item.disabled || !item.submenu.length}
        onpointerenter={(e) => openSub(i, item, e.currentTarget)}
        onclick={(e) => openSub(i, item, e.currentTarget)}
        onkeydown={(e) => {
          if (e.key === 'ArrowRight' || e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            enterSub(i, item, e.currentTarget);
          }
        }}
      >
        {item.label}
        <span class="caret" aria-hidden="true">›</span>
      </button>
    {:else}
      <button class="item" class:danger={item.danger} role="menuitem" disabled={item.disabled} onpointerenter={closeSubSoon} onclick={() => pick(item)}>
        {item.label}
        {#if item.hint}<span class="hint">{item.hint}</span>{/if}
      </button>
    {/if}
  {/each}

  {#if sub}
    <div
      class="menu sub"
      role="menu"
      tabindex="-1"
      bind:this={subEl}
      style="left: {subPos.left}px; top: {subPos.top}px"
      onpointerenter={keepSub}
      onkeydown={(e) => e.key === 'ArrowLeft' && (e.stopPropagation(), leaveSub())}
    >
      {#each sub.items as item}
        {#if item === 'separator'}
          <div class="sep"></div>
        {:else}
          <button class="item" class:danger={item.danger} role="menuitem" disabled={item.disabled} onclick={() => pick(item)}>
            {item.label}
            {#if item.hint}<span class="hint">{item.hint}</span>{/if}
          </button>
        {/if}
      {/each}
    </div>
  {/if}
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
  /* The nested menu is fixed to the viewport, so the parent's scroll box doesn't clip it. */
  .menu.sub {
    z-index: 101;
    min-width: 180px;
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
  .item:hover:not(:disabled),
  .item.open {
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
  .caret {
    color: var(--text-faint);
    font-size: 15px;
    line-height: 1;
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
