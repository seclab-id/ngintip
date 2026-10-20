<script>
  import { tick } from 'svelte';
  import { highlightHttp } from '../lib/highlight.js';
  import { matchRanges, markHtmlRanges } from '../lib/find.js';
  import { autoDecode } from '../lib/codec.js';
  import FindBar from './FindBar.svelte';

  // Always-editable HTTP editor with syntax colours. A transparent textarea sits on
  // top of a highlighted <pre> that mirrors it exactly, so the whole message stays
  // freely editable while method, header names, status and JSON/markup are coloured
  // underneath. Both layers live inside ONE scroll container (`.scroll`) and are the
  // same size, so they wrap identically (the scrollbar narrows both equally) and
  // there is a single scroll position — no JS scroll-syncing, so the caret always
  // lands exactly where the coloured text shows it.
  // `onselect(text)` fires with the current selection (Burp-style) so a parent can
  // open a decoder side bar for it.
  let { value = $bindable(''), onselect = null } = $props();

  let ta = $state();
  let layer = $state();
  let scroller = $state();
  let root = $state();
  let hovered = $state(false);

  // Find/search: while open with a query, the colour layer layers its match
  // <mark>s ON TOP of the syntax colours (via text-offset ranges), so the message
  // stays coloured while searching, aligned with the transparent textarea.
  let findOpen = $state(false);
  let query = $state('');
  let matchIdx = $state(0);
  let matches = $derived(findOpen && query ? matchRanges(value, query) : null);
  let count = $derived(matches?.count ?? 0);

  // A trailing newline leaves an empty final line; a space keeps it rendered so
  // the caret there lines up with the colour layer.
  let html = $derived((count ? markHtmlRanges(highlightHttp(value), matches.ranges, matchIdx) : highlightHttp(value)) + (value === '' || value.endsWith('\n') ? ' ' : ''));

  // Keep the active match in range and scroll it into view (the shared container
  // scrolls both layers, so the caret and colours stay together).
  $effect(() => {
    if (!findOpen) return;
    if (count && matchIdx >= count) matchIdx = 0;
    if (!count) return;
    matchIdx;
    tick().then(() => {
      const m = layer?.querySelector('mark.mk.on');
      if (m && scroller) {
        const mr = m.getBoundingClientRect();
        const sr = scroller.getBoundingClientRect();
        scroller.scrollTop += mr.top - sr.top - scroller.clientHeight / 2 + mr.height / 2;
      }
    });
  });

  // Report the current selection to the parent, but only when it actually looks
  // decodable, so selecting ordinary text does not pop the decoder side bar.
  function reportSelection() {
    if (!onselect || !ta) return;
    const s = ta.value.slice(ta.selectionStart, ta.selectionEnd);
    if (s.trim() && s.length <= 100_000 && autoDecode(s).length) onselect(s);
  }

  function step(d) {
    if (count) matchIdx = (matchIdx + d + count) % count;
  }
  // Called from Cmd/Ctrl+F (when hovered/focused) or a parent Find button.
  export function openFind() {
    findOpen = true;
    matchIdx = 0;
  }
  function closeFind() {
    findOpen = false;
    query = '';
    ta?.focus();
  }
  $effect(() => {
    const handler = (e) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'f' || e.key === 'F')) {
        if (hovered || root?.contains(document.activeElement)) {
          e.preventDefault();
          openFind();
        }
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="wrap" bind:this={root} onpointerenter={() => (hovered = true)} onpointerleave={() => (hovered = false)}>
  <div class="scroll" bind:this={scroller}>
    <!-- `.grow` sizes to the content (the <pre> in normal flow); the textarea is
         absolutely stretched over it, so both are identical in size and wrap. -->
    <div class="grow">
      <pre class="layer" aria-hidden="true" bind:this={layer}>{@html html}</pre>
      <textarea
        class="editor"
        bind:this={ta}
        bind:value
        onmouseup={reportSelection}
        onkeyup={reportSelection}
        spellcheck="false"
        autocomplete="off"
        autocapitalize="off"
        autocorrect="off"
        wrap="soft"
      ></textarea>
    </div>
  </div>
  {#if findOpen}
    <FindBar bind:value={query} {count} index={matchIdx} oninput={() => (matchIdx = 0)} onstep={step} onclose={closeFind} />
  {/if}
</div>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    flex: 1;
    /* Safety net: stay usable even if a flex/grid parent fails to give height. */
    min-height: 180px;
    min-width: 0;
  }
  /* The ONE scroll container: its scrollbar narrows both layers together, so they
     can never wrap differently, and there is a single scroll position to track. */
  .scroll {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  /* Grows to the content height (from the <pre>), but at least fills the viewport so
     clicking in the empty area still places the caret. */
  .grow {
    position: relative;
    min-height: 100%;
  }
  /* Both layers must render text identically for the overlay to line up: same box,
     font, wrapping, padding and tab size. */
  .layer,
  .editor {
    box-sizing: border-box;
    margin: 0;
    padding: 12px 14px;
    border: 0;
    font: 12.5px/1.6 var(--mono);
    white-space: pre-wrap;
    word-break: break-word;
    tab-size: 4;
  }
  .layer {
    /* In normal flow: this is what gives `.grow` (and thus the textarea) its height. */
    position: relative;
    display: block;
    color: var(--text);
    pointer-events: none;
  }
  .layer :global(mark.mk) {
    padding: 0;
    border-radius: 2px;
    background: color-mix(in srgb, var(--amber) 40%, transparent);
    color: inherit;
  }
  .layer :global(mark.mk.on) {
    background: var(--amber);
    color: #1b1300;
  }
  .editor {
    /* Stretched over `.grow` (top+bottom+left+right:0), so it is exactly the same
       size as the <pre> even though `.grow` has an auto (content) height. It never
       scrolls itself — the shared `.scroll` container does. */
    position: absolute;
    inset: 0;
    resize: none;
    outline: none;
    overflow: hidden;
    background: transparent;
    color: transparent;
    caret-color: var(--text);
    -webkit-text-fill-color: transparent;
    user-select: text;
    -webkit-user-select: text;
  }
  .editor::selection {
    background: var(--selection);
  }
  .editor::placeholder {
    color: var(--text-faint);
  }
</style>
