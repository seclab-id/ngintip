<script>
  import { DECODERS, autoDecode } from '../lib/codec.js';
  import { store } from '../lib/store.svelte.js';
  import { brandKey } from '../lib/brand.js';

  // A small inline decoder for a highlighted snippet from a message viewer.
  let { text = '', onclose } = $props();

  // Draggable, remembered width (per machine) so it survives switching tabs/views
  // and reopening. The dock column sizes to this width (see parent `.with-decoder`).
  const WKEY = brandKey('decoder.width');
  const MIN_W = 220;
  const MAX_W = 900;
  function loadWidth() {
    try {
      const v = parseInt(localStorage.getItem(WKEY), 10);
      if (v >= MIN_W && v <= MAX_W) return v;
    } catch {
      // storage unavailable; use the default
    }
    return 320;
  }
  let width = $state(loadWidth());

  function startResize(e) {
    const startX = e.clientX;
    const startW = width;
    e.currentTarget.setPointerCapture?.(e.pointerId);
    const max = () => Math.min(MAX_W, Math.round((window.innerWidth || 1200) * 0.6));
    const move = (ev) => {
      // Handle is on the left edge; dragging left widens the right-hand dock.
      width = Math.min(max(), Math.max(MIN_W, startW + (startX - ev.clientX)));
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      try {
        localStorage.setItem(WKEY, String(width));
      } catch {
        // ignore; width still applies for this session
      }
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    e.preventDefault();
  }

  // Auto-detect on open; the user can then pick a specific decoder.
  let candidates = $derived(autoDecode(text));
  let choice = $state('auto');

  // Reset the picker each time a new snippet is decoded.
  $effect(() => {
    text;
    choice = 'auto';
  });

  let result = $derived.by(() => {
    const src = (text ?? '').trim();
    if (!src) return { label: '', output: '', error: '' };
    if (choice === 'auto') {
      const best = candidates[0];
      return best
        ? { label: `${best.label} (auto)`, output: best.output, error: '' }
        : { label: '', output: '', error: 'No decoder matched this selection.' };
    }
    const d = DECODERS.find((x) => x.id === choice);
    if (!d) return { label: '', output: '', error: '' };
    try {
      return { label: d.label, output: d.run(src), error: '' };
    } catch (e) {
      return { label: d.label, output: '', error: e.message ?? String(e) };
    }
  });

  let copied = $state(false);
  async function copy() {
    if (!result.output) return;
    await navigator.clipboard.writeText(result.output);
    copied = true;
    setTimeout(() => (copied = false), 1200);
  }
</script>

<div class="mini" style="width: {width}px">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="resize" title="Drag to resize" onpointerdown={startResize}></div>
  <header class="mini-head">
    <span class="mini-title">Decode</span>
    <div class="spacer"></div>
    <button class="btn ghost sm" onclick={() => store.sendStringToDecoder(text)}>Open in Decoder</button>
    <button class="x" title="Close" aria-label="Close" onclick={() => onclose?.()}>×</button>
  </header>

  <div class="mini-body">
    <label class="row">
      <span class="lbl">As</span>
      <select class="input sel" bind:value={choice}>
        <option value="auto">Auto{candidates.length ? ` — ${candidates[0].label}` : ''}</option>
        {#each DECODERS as d}
          <option value={d.id}>{d.label}</option>
        {/each}
      </select>
    </label>

    <div class="section">
      <div class="cap">Selection ({text.length} chars)</div>
      <pre class="snippet">{text}</pre>
    </div>

    <div class="section grow">
      <div class="cap">
        Output{result.label ? ` · ${result.label}` : ''}
        <div class="spacer"></div>
        <button class="btn ghost sm" onclick={copy} disabled={!result.output}>{copied ? 'Copied ✓' : 'Copy'}</button>
      </div>
      {#if result.error}
        <pre class="snippet err">{result.error}</pre>
      {:else}
        <pre class="snippet out">{result.output}</pre>
      {/if}
    </div>

    {#if candidates.length > 1 && choice === 'auto'}
      <div class="others">
        <span class="cap">Other matches:</span>
        {#each candidates.slice(1) as c}
          <button class="chip" onclick={() => (choice = c.id)}>{c.label}</button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .mini {
    position: relative;
    display: flex;
    flex-direction: column;
    min-width: 0;
    height: 100%;
    border-left: 1px solid var(--border);
    background: var(--bg-elev);
  }
  /* Drag handle sitting on the left border, between the message panes and the dock. */
  .resize {
    position: absolute;
    top: 0;
    bottom: 0;
    left: -3px;
    z-index: 2;
    width: 7px;
    cursor: col-resize;
    touch-action: none;
  }
  .resize:hover {
    background: color-mix(in srgb, var(--accent) 45%, transparent);
  }
  .mini-head {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 34px;
    padding: 0 8px;
    border-bottom: 1px solid var(--border);
  }
  .mini-title {
    font: 600 12px var(--sans);
    color: var(--text);
  }
  .spacer {
    flex: 1;
  }
  .x {
    width: 22px;
    height: 22px;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font-size: 16px;
    cursor: pointer;
  }
  .x:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .mini-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
    padding: 8px;
    overflow: auto;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .lbl {
    font-size: 11.5px;
    color: var(--text-dim);
  }
  .sel {
    flex: 1;
    height: 26px;
  }
  .section {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-height: 0;
  }
  .section.grow {
    flex: 1;
  }
  .cap {
    display: flex;
    align-items: center;
    font: 600 10.5px var(--sans);
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .snippet {
    margin: 0;
    padding: 6px 8px;
    max-height: 120px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--text);
    font: 12px var(--mono);
    white-space: pre-wrap;
    word-break: break-word;
  }
  .snippet.out {
    flex: 1;
    max-height: none;
  }
  .snippet.err {
    color: var(--red);
  }
  .others {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .chip {
    height: 22px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 500 11px var(--sans);
    cursor: pointer;
  }
  .chip:hover {
    color: var(--text);
    border-color: var(--accent);
  }
</style>
