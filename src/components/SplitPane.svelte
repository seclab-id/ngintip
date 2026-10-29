<script>
  // Two panes with a draggable divider: side by side, or stacked top/bottom with
  // `vertical`. The ratio is remembered per `storeKey` in localStorage.
  import { brandKey } from '../lib/brand.js';
  let { left, right, min = 0.15, max = 0.85, start = 0.5, storeKey, vertical = false } = $props();

  function load() {
    try {
      const v = storeKey && parseFloat(localStorage.getItem(brandKey(`split.${storeKey}`)));
      if (v && v >= min && v <= max) return v;
    } catch {
      // storage unavailable; use the default
    }
    return start;
  }

  let split = $state(load());
  let el;
  let dragging = false;

  function down(e) {
    dragging = true;
    e.currentTarget.setPointerCapture(e.pointerId);
  }
  function move(e) {
    if (!dragging) return;
    const r = el.getBoundingClientRect();
    const f = vertical ? (e.clientY - r.top) / r.height : (e.clientX - r.left) / r.width;
    split = Math.min(max, Math.max(min, f));
  }
  function up() {
    dragging = false;
    try {
      if (storeKey) localStorage.setItem(brandKey(`split.${storeKey}`), String(split));
    } catch {
      // not critical
    }
  }
</script>

<div
  class="split"
  class:vertical
  bind:this={el}
  style="{vertical ? 'grid-template-rows' : 'grid-template-columns'}: {split}fr 6px {1 - split}fr"
>
  <div class="pane">{@render left()}</div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="divider"
    role="separator"
    aria-orientation={vertical ? 'horizontal' : 'vertical'}
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    ondblclick={() => (split = start)}
  ></div>
  <div class="pane">{@render right()}</div>
</div>

<style>
  .split {
    display: grid;
    min-width: 0;
    min-height: 0;
  }
  .pane {
    display: grid;
    min-width: 0;
    min-height: 0;
  }
  .divider {
    background: var(--border);
    cursor: col-resize;
  }
  .vertical > .divider {
    cursor: row-resize;
  }
  .divider:hover {
    background: var(--accent);
  }
</style>
