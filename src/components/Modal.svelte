<script>
  // A centered modal dialog. Close via the X button, the Escape key, or a click
  // on the backdrop. Content is a child snippet. With `draggable`, the title bar
  // can be grabbed to move the modal around.
  let { title = '', onclose, draggable = false, children } = $props();

  let dx = $state(0);
  let dy = $state(0);
  let drag = null; // { sx, sy, ox, oy }

  function down(e) {
    if (!draggable || e.target.closest('button')) return; // let the X button click
    drag = { sx: e.clientX, sy: e.clientY, ox: dx, oy: dy };
    try {
      e.currentTarget.setPointerCapture?.(e.pointerId);
    } catch {
      /* jsdom / non-pointer events */
    }
  }
  function move(e) {
    if (!drag) return;
    // Keep it from being dragged fully off-screen (soft clamp to half a viewport).
    const mx = window.innerWidth / 2;
    const my = window.innerHeight / 2;
    dx = Math.max(-mx, Math.min(mx, drag.ox + (e.clientX - drag.sx)));
    dy = Math.max(-my, Math.min(my, drag.oy + (e.clientY - drag.sy)));
  }
  function up() {
    drag = null;
  }
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div class="backdrop" role="presentation" onmousedown={(e) => e.target === e.currentTarget && onclose()}>
  <div class="modal" role="dialog" aria-modal="true" aria-label={title} style="transform: translate({dx}px, {dy}px)">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <header class="mhead" class:draggable onpointerdown={down} onpointermove={move} onpointerup={up}>
      <span class="mtitle">{title}</span>
      <button class="x" title="Close (Esc)" aria-label="Close" onclick={onclose}>×</button>
    </header>
    <div class="mbody">
      {@render children?.()}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 200;
    display: grid;
    place-items: center;
    padding: 24px;
    background: color-mix(in srgb, black 48%, transparent);
  }
  .modal {
    display: flex;
    flex-direction: column;
    width: min(660px, 100%);
    max-height: min(82vh, 760px);
    border: 1px solid var(--border);
    border-radius: 12px;
    background: var(--bg-elev);
    box-shadow: 0 18px 50px rgba(0, 0, 0, 0.45);
    overflow: hidden;
  }
  .mhead {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 11px 14px;
    border-bottom: 1px solid var(--border);
  }
  .mhead.draggable {
    cursor: move;
    user-select: none;
    touch-action: none;
  }
  .mtitle {
    flex: 1;
    overflow: hidden;
    font: 600 13px var(--sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .x {
    width: 26px;
    height: 26px;
    padding: 0;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text-dim);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
  }
  .x:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .mbody {
    padding: 14px;
    overflow: auto;
  }
</style>
