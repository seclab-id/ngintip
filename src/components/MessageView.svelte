<script>
  import { tick } from 'svelte';
  import { highlightHttp } from '../lib/highlight.js';
  import { prettyBody, htmlBody, softWrapLong } from '../lib/message.js';
  import { matchRanges, markHtmlRanges } from '../lib/find.js';
  import { autoDecode } from '../lib/codec.js';
  import { fmtSize } from '../lib/format.js';
  import FindBar from './FindBar.svelte';

  // The read-only viewer colours much larger bodies than the editable editor, so a
  // big pretty-printed JSON/XML response keeps its syntax colours.
  const VIEWER_HL_LIMIT = 2_000_000;

  // `truncated` + `loadFull` enable a "Show all" button that fetches the full,
  // uncapped body from the backend (bodies are stored in full, only the view is capped).
  let { title, raw = '', image = null, actions, placeholder = '', truncated = false, loadFull = null,
    ondecode = null, onsend = null, variants = null, isImage = false, loadImage = null,
    isBinary = false, loadHex = null, preferPretty = false } = $props();

  // Image preview: use an eagerly-supplied `image`, else lazy-load via `loadImage`
  // the first time Preview is opened (so opening a flow never encodes an image it
  // won't display).
  let lazyImage = $state(null);
  let previewImage = $derived(image ?? lazyImage);
  let canPreviewImage = $derived(!!(image || (isImage && loadImage)));

  // Default to Pretty when asked (Repeater): activeMode still falls back to Raw for a
  // body that can't be prettified (plain text, binary), and the user can toggle freely.
  // Only the initial value of this constant prop is needed, by design.
  // svelte-ignore state_referenced_locally
  let mode = $state(preferPretty ? 'pretty' : 'raw');
  let showingFull = $state(false);
  let full = $state(null);
  let loadingFull = $state(false);
  // A binary (non-printable) response body isn't rendered in Raw until asked — a
  // large image/blob would otherwise lag the viewer just by highlighting it.
  let bodyShown = $state(false);
  // Which captured version is shown: 'final' (the sent/stored message) or an earlier
  // one (Original / Extension). Only History passes `variants`.
  let variantId = $state('final');
  let showVariants = $derived(Array.isArray(variants) && variants.length > 1);
  let variantRaw = $derived.by(() => {
    if (!showVariants) return null;
    const v = variants.find((x) => x.id === variantId);
    return v && v.id !== 'final' ? v.raw : null;
  });

  // Reset the expanded/variant state whenever a different message is shown.
  $effect(() => {
    raw;
    showingFull = false;
    full = null;
    variantId = 'final';
    lazyImage = null;
    loadingImage = false;
    lazyHex = null;
    loadingHex = false;
    bodyShown = false;
  });

  // Fetch the image only when Preview is first opened for a lazy image flow.
  let loadingImage = $state(false);
  $effect(() => {
    if (activeMode === 'preview' && isImage && !image && lazyImage == null && !loadingImage && loadImage) {
      loadingImage = true;
      Promise.resolve(loadImage())
        .then((d) => (lazyImage = d))
        .finally(() => (loadingImage = false));
    }
  });

  // Hex (Pretty) view for a binary body: only offered for the final message (not a
  // captured variant), and fetched on demand the first time Pretty is opened.
  let lazyHex = $state(null);
  let loadingHex = $state(false);
  let hexAvailable = $derived(!!(isBinary && loadHex && variantId === 'final'));
  $effect(() => {
    if (activeMode === 'pretty' && hexAvailable && !pretty && lazyHex == null && !loadingHex) {
      loadingHex = true;
      Promise.resolve(loadHex())
        .then((h) => (lazyHex = h))
        .finally(() => (loadingHex = false));
    }
  });

  // A non-final variant is already the full captured text (no "Show all").
  let base = $derived(variantRaw != null ? variantRaw : showingFull && full != null ? full : raw);
  // A binary body's Raw view is deferred behind "Load content" (final message only).
  let deferBinary = $derived(isBinary && variantId === 'final' && !bodyShown);
  // Head (start line + headers) without the body, for the deferred view.
  let headText = $derived.by(() => {
    const i = (base ?? '').indexOf('\n\n');
    return i === -1 ? (base ?? '') : base.slice(0, i + 2);
  });
  let bodyBytes = $derived(Math.max(0, (base?.length ?? 0) - headText.length));
  // Pretty formats JSON/XML/HTML; a binary body uses the hex dump (lazy) instead, so
  // skip the pretty/HTML scans entirely for binary (they'd chew through the blob).
  let pretty = $derived(base && !isBinary ? prettyBody(base) : null);
  let html = $derived(base && !canPreviewImage && !isBinary ? htmlBody(base) : null);
  let modes = $derived([
    { id: 'raw', label: 'Raw', on: true },
    { id: 'pretty', label: hexAvailable && !pretty ? 'Hex' : 'Pretty', on: !!pretty || hexAvailable },
    { id: 'preview', label: 'Preview', on: !!(canPreviewImage || html) },
  ]);
  let activeMode = $derived(modes.find((m) => m.id === mode)?.on ? mode : 'raw');
  let shown = $derived.by(() => {
    if (activeMode === 'pretty') {
      if (pretty) return pretty;
      if (hexAvailable) return lazyHex ?? 'Loading hex view...';
      return base;
    }
    // Raw view: for a binary body, render only the head until the user loads it.
    if (deferBinary) return headText;
    return base;
  });
  // Search over the shown text.
  let searchOpen = $state(false);
  let find = $state('');
  let findOpts = $state({ caseSensitive: false, regex: false, word: false });
  let matchIdx = $state(0);
  let pre = $state();
  let root = $state();
  let hovered = $state(false);

  // Open fast even for a huge message: render only a preview initially. "Show all"
  // (or opening search, which needs the whole text) expands to the full captured
  // message. Copy/Capture still use the full `shown`, not the preview.
  const PREVIEW_CHARS = 60_000;
  let previewing = $derived(!showingFull && !searchOpen && (shown?.length ?? 0) > PREVIEW_CHARS);
  // What gets rendered/searched: long lines are chunked so a single 400 KB line
  // (an inline data: URI, a minified blob) can't freeze the webview's layout.
  let view = $derived(
    softWrapLong(
      previewing
        ? `${shown.slice(0, PREVIEW_CHARS)}\n\n[preview — showing the first ${Math.round(PREVIEW_CHARS / 1000)} KB for speed; use "Show all" for the full message]`
        : (shown ?? ''),
    ),
  );

  // Debounce the query/options so typing stays smooth on large bodies (the match +
  // re-highlight pass only runs after a short pause, not on every keystroke).
  let dsearch = $state({ q: '', opts: { caseSensitive: false, regex: false, word: false } });
  $effect(() => {
    const q = find;
    const o = { caseSensitive: findOpts.caseSensitive, regex: findOpts.regex, word: findOpts.word };
    // Small bodies update instantly; only large ones debounce (that's where the match +
    // re-highlight pass is expensive enough to make typing feel laggy).
    if ((view?.length ?? 0) <= 20000) { dsearch = { q, opts: o }; return; }
    const t = setTimeout(() => (dsearch = { q, opts: o }), 110);
    return () => clearTimeout(t);
  });
  // Find layers its <mark>s ON TOP of the syntax-highlighted HTML (via text-offset
  // ranges) so the colours stay while searching, instead of replacing them with
  // plain marked text.
  let searchResult = $derived(searchOpen && dsearch.q ? matchRanges(view, dsearch.q, dsearch.opts) : null);
  // This is a read-only viewer, so colour even large bodies: a body that is under
  // the limit as raw can exceed it once Pretty adds whitespace, and it should keep
  // its colours. The view is bounded (raw <= VIEW_LIMIT, pretty of <=200 KB), so
  // this cannot receive an unbounded string.
  let highlighted = $derived(highlightHttp(view, VIEWER_HL_LIMIT));
  let body = $derived(searchResult?.count ? markHtmlRanges(highlighted, searchResult.ranges, matchIdx) : highlighted);

  // Keep the active match in range and scrolled into view.
  $effect(() => {
    const count = searchResult?.count ?? 0;
    if (count && matchIdx >= count) matchIdx = 0;
    if (!count) return;
    matchIdx;
    tick().then(() => pre?.querySelector('mark.mk.on')?.scrollIntoView?.({ block: 'center' }));
  });

  function step(d) {
    const count = searchResult?.count ?? 0;
    if (count) matchIdx = (matchIdx + d + count) % count;
  }
  // The text exactly as this pane currently shows it (Raw / Pretty / Hex, and any
  // "Show all"), so a Capture can mirror the chosen view instead of always the raw.
  // `null` in Preview (image) mode, where there is no text to render.
  export function displayText() {
    return activeMode === 'preview' ? null : shown;
  }
  export function viewMode() {
    return activeMode;
  }
  // The fraction of the body currently scrolled into view ({top, bottom} in 0..1), or
  // null when it all fits (nothing scrolled). Used by Capture's "follow page scroll".
  export function visibleFraction() {
    const el = pre;
    if (!el || el.scrollHeight <= el.clientHeight + 2) return null;
    return { top: el.scrollTop / el.scrollHeight, bottom: (el.scrollTop + el.clientHeight) / el.scrollHeight };
  }
  // Called from Cmd/Ctrl+F (when this pane is hovered/focused), a Find button, or
  // "Open in History" for a passive finding (which passes the match to highlight).
  export function openFind(query = null) {
    if (!raw || activeMode === 'preview') return;
    if (typeof query === 'string' && query) find = query;
    searchOpen = true;
    matchIdx = 0;
  }
  function closeFind() {
    searchOpen = false;
    find = '';
  }
  // Cmd/Ctrl+F opens the find bar for whichever message pane is under the pointer
  // or holds focus (so it works without the pane being a focusable element).
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

  async function showAll() {
    if (showingFull) {
      showingFull = false; // back to the fast preview
      return;
    }
    // Only fetch the uncapped body from the backend when the final message is
    // actually truncated; a preview or a captured variant already holds the whole text.
    if (truncated && variantId === 'final' && full == null && loadFull) {
      loadingFull = true;
      try {
        full = await loadFull();
      } finally {
        loadingFull = false;
      }
    }
    showingFull = true;
  }

  let copied = $state(false);
  async function copy() {
    await navigator.clipboard.writeText(shown);
    copied = true;
    setTimeout(() => (copied = false), 1200);
  }

  // Highlight-to-decode: selecting text in the body opens the decoder side bar
  // directly (Burp-style), via the parent's `ondecode` callback.
  function selectionText() {
    const s = window.getSelection?.();
    if (!s || s.isCollapsed) return '';
    if (pre && s.anchorNode && !pre.contains(s.anchorNode)) return '';
    return s.toString();
  }

  function onSelectUp() {
    if (!ondecode) return;
    const text = selectionText();
    // Only open the decoder when the selection actually looks decodable (JWT,
    // Base64, URL/percent, hex, HTML entities). Selecting ordinary text - e.g. to
    // copy it - no longer pops the side bar and shifts the panes.
    if (text.trim() && text.length <= 100_000 && autoDecode(text).length) ondecode(text);
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="panel" bind:this={root} onpointerenter={() => (hovered = true)} onpointerleave={() => (hovered = false)}>
  <header class="panel-head">
    <span class="panel-title">{title}</span>
    {#if raw}
      <div class="modes" role="tablist">
        {#each modes as m}
          <button class="mode" class:active={activeMode === m.id} role="tab" aria-selected={activeMode === m.id} disabled={!m.on} onclick={() => (mode = m.id)}>
            {m.label}
          </button>
        {/each}
      </div>
    {/if}
    {#if showVariants}
      <div class="modes variants" role="tablist" title="Which captured version to show">
        {#each variants as v}
          <button class="mode" class:active={variantId === v.id} role="tab" aria-selected={variantId === v.id} onclick={() => (variantId = v.id)}>{v.label}</button>
        {/each}
      </div>
    {/if}
    <div class="spacer"></div>
    {@render actions?.()}
    {#if raw && (previewing || showingFull || (variantId === 'final' && truncated))}
      <button class="btn ghost sm" onclick={showAll} disabled={loadingFull}>
        {loadingFull ? 'Loading…' : showingFull ? 'Show less' : 'Show all'}
      </button>
    {/if}
    {#if raw && activeMode !== 'preview'}
      <button class="btn ghost sm icon-btn" title="Search (Cmd/Ctrl+F)" aria-label="Search" onclick={openFind}>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="7" /><line x1="21" y1="21" x2="16.65" y2="16.65" /></svg>
      </button>
      <button class="btn ghost sm" onclick={copy}>{copied ? 'Copied ✓' : 'Copy'}</button>
    {/if}
  </header>

  {#if !raw}
    <div class="placeholder">{placeholder}</div>
  {:else if activeMode === 'preview' && previewImage}
    <div class="image"><img src={previewImage} alt="Response preview" /></div>
  {:else if activeMode === 'preview' && canPreviewImage}
    <div class="placeholder">Loading image…</div>
  {:else if activeMode === 'preview'}
    <!-- Empty sandbox: no scripts, forms, or same-origin access for the target's HTML. -->
    <iframe class="frame" title="{title} preview" sandbox="" srcdoc={html}></iframe>
  {:else}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_no_noninteractive_element_interactions -->
    <pre class="code" class:head-only={deferBinary} bind:this={pre} onmouseup={onSelectUp}>{@html body}</pre>
    {#if deferBinary}
      <div class="load-body">
        <span class="dim">Binary body ({fmtSize(bodyBytes)}, not printable) — hidden to keep the viewer fast.</span>
        <div class="load-actions">
          {#if canPreviewImage}
            <button class="btn sm" onclick={() => (mode = 'preview')}>Preview image</button>
          {/if}
          {#if hexAvailable}
            <button class="btn ghost sm" onclick={() => (mode = 'pretty')}>View hex</button>
          {/if}
          <button class="btn ghost sm" onclick={() => (bodyShown = true)}>Load raw</button>
        </div>
      </div>
    {/if}
    {#if searchOpen}
      <FindBar bind:value={find} bind:opts={findOpts} error={searchResult?.error} count={searchResult?.count ?? 0} index={matchIdx} placeholder="Search body…"
        oninput={() => (matchIdx = 0)} onstep={step} onclose={closeFind} />
    {/if}
  {/if}
</div>

<style>
  .panel {
    position: relative;
  }
  pre {
    flex: 1;
    margin: 0;
    overflow: auto;
  }
  /* When a binary body is deferred, the head shrinks to content and the load
     placeholder fills the rest. */
  pre.head-only {
    flex: 0 0 auto;
    max-height: 45%;
  }
  .load-body {
    flex: 1;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 12px;
    padding: 16px;
    text-align: center;
  }
  .load-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    gap: 8px;
  }
  pre :global(mark.mk) {
    padding: 0;
    border-radius: 2px;
    background: color-mix(in srgb, var(--amber) 40%, transparent);
    color: inherit;
  }
  pre :global(mark.mk.on) {
    background: var(--amber);
    color: #1b1300;
  }
  .modes {
    display: flex;
    gap: 2px;
    margin-left: 10px;
    padding: 2px;
    border-radius: 7px;
    background: var(--bg-soft);
  }
  /* The version switcher (Final / Original / Extension) gets an accent tint so it
     reads as distinct from the Raw/Pretty/Preview modes. */
  .modes.variants {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-soft));
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 30%, transparent);
  }
  .mode {
    height: 22px;
    padding: 0 9px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font: 500 11.5px var(--sans);
    cursor: pointer;
  }
  .mode:hover:not(:disabled) {
    color: var(--text);
  }
  .mode.active {
    background: var(--bg-elev);
    color: var(--text);
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.25);
  }
  .mode:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .frame {
    flex: 1;
    border: 0;
    background: #fff;
  }
  .image {
    display: grid;
    flex: 1;
    place-items: center;
    padding: 16px;
    overflow: auto;
    background: repeating-conic-gradient(var(--bg-soft) 0 25%, transparent 0 50%) 0 0 / 16px 16px;
  }
  .image img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }
  .placeholder {
    display: grid;
    flex: 1;
    place-items: center;
    color: var(--text-faint);
    font-size: 12.5px;
  }
  .icon-btn {
    display: inline-grid;
    place-items: center;
    padding: 0 7px;
  }
  .icon-btn svg {
    display: block;
  }
</style>
