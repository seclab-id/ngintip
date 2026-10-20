<script>
  // Two side-by-side panes with a draggable vertical divider. The ratio is
  // remembered per `storeKey` in localStorage.
  import { brandKey } from '../lib/brand.js';
  let { left, right, min = 0.15, max = 0.85, start = 0.5, storeKey } = $props();

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
    split = Math.min(max, Math.max(min, (e.clientX - r.left) / r.width));
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

<div class="split" bind:this={el} style="grid-template-columns: {split}fr 6px {1 - split}fr">
  <div class="pane">{@render left()}</div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="divider"
    role="separator"
    aria-orientation="vertical"
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
  .divider:hover {
    background: var(--accent);
  }
</style>
