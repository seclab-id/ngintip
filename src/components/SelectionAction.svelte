<script>
  // A small "Decode" button that appears next to a selection, instead of the decoder
  // dock opening by itself.
  //
  // The dock used to open the moment a selection looked decodable. That is wrong twice
  // over: most selections are made to copy something, and the dock is a grid column, so
  // opening it reflows the panes under the pointer while the operator is still reading.
  // Highlighting is now just highlighting; this chip is the only thing that appears, and
  // nothing moves until it is clicked.
  //
  // Positioned like ContextMenu: fixed to the viewport, pulled back inside it after
  // mount, and closed by anything that would leave it stranded (a mousedown elsewhere,
  // Escape, a scroll, a resize, the window losing focus).
  import { onMount } from 'svelte';

  let { x, y, hint = '', onpick, onclose } = $props();

  let el = $state();
  let overflow = $state({ x: 0, y: 0 });
  let left = $derived(x - overflow.x);
  let top = $derived(y - overflow.y);

  onMount(() => {
    const r = el.getBoundingClientRect();
    overflow = {
      x: Math.max(0, x + r.width + 6 - window.innerWidth),
      // Flip above the pointer rather than just shifting up, so the chip never lands on
      // top of the text the selection ends at.
      y: y + r.height + 6 > window.innerHeight ? r.height + 24 : 0,
    };
    // A scroll event does not bubble, so a listener on the window never hears the pane
    // the text is in. Capture does hear it: the event still travels down to the target.
    // Without this the chip hangs in mid-air over whatever scrolled past underneath.
    window.addEventListener('scroll', onclose, true);
    return () => window.removeEventListener('scroll', onclose, true);
  });
</script>

<svelte:window
  onmousedown={(e) => !el?.contains(e.target) && onclose()}
  onresize={onclose}
  onblur={onclose}
  onkeydown={(e) => e.key === 'Escape' && onclose()}
/>

<button
  class="chip"
  bind:this={el}
  style="left: {left}px; top: {top}px"
  title={hint ? `Open the decoder (looks like ${hint})` : 'Open the decoder for the selected text'}
  onmousedown={(e) => e.preventDefault()}
  onclick={onpick}
>
  Decode
  {#if hint}<span class="hint">{hint}</span>{/if}
</button>

<style>
  .chip {
    position: fixed;
    z-index: 100;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-elev);
    color: var(--text);
    font: 11.5px var(--sans);
    white-space: nowrap;
    cursor: pointer;
    box-shadow: 0 6px 18px rgb(0 0 0 / 0.3);
  }
  .chip:hover {
    background: var(--accent-soft);
  }
  .hint {
    color: var(--text-faint);
  }
</style>
