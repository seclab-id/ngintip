<script>
  // A compact find bar shown at the bottom of a searchable pane. It only renders
  // the input + match counter + prev/next/close controls; the parent owns the
  // matching/highlighting (see lib/find.js) and reacts to onstep/onclose/oninput.
  let {
    value = $bindable(''),
    count = 0,
    index = 0,
    placeholder = 'Search…',
    opts = $bindable({ caseSensitive: false, regex: false, word: false }),
    error = false,
    oninput,
    onstep,
    onclose,
  } = $props();
  let input = $state();
  function toggle(k) { opts[k] = !opts[k]; oninput?.(); }

  export function focus() {
    input?.focus();
    input?.select();
  }
  // Focus the input as soon as the bar appears.
  $effect(() => {
    input?.focus();
  });
</script>

<div class="find">
  <input
    bind:this={input}
    class="find-input"
    bind:value
    {placeholder}
    spellcheck="false"
    oninput={() => oninput?.()}
    onkeydown={(e) => {
      if (e.key === 'Enter') {
        e.preventDefault();
        onstep?.(e.shiftKey ? -1 : 1);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        onclose?.();
      }
    }}
  />
  <div class="find-togs">
    <button class="find-tog" class:on={opts.caseSensitive} title="Match case" aria-pressed={opts.caseSensitive} onclick={() => toggle('caseSensitive')}>Aa</button>
    <button class="find-tog" class:on={opts.word} title="Whole word" aria-pressed={opts.word} onclick={() => toggle('word')}>\b</button>
    <button class="find-tog" class:on={opts.regex} title="Regular expression" aria-pressed={opts.regex} onclick={() => toggle('regex')}>.*</button>
  </div>
  <span class="find-count" class:err={error}>{error ? 'bad regex' : count ? `${index + 1}/${count}` : value ? '0/0' : ''}</span>
  <button class="find-btn" title="Previous (Shift+Enter)" aria-label="Previous match" onclick={() => onstep?.(-1)} disabled={!count}>‹</button>
  <button class="find-btn" title="Next (Enter)" aria-label="Next match" onclick={() => onstep?.(1)} disabled={!count}>›</button>
  <button class="find-btn" title="Close (Esc)" aria-label="Close search" onclick={() => onclose?.()}>×</button>
</div>

<style>
  .find {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 5px 8px;
    border-top: 1px solid var(--border);
    background: var(--bg-elev);
  }
  .find-input {
    flex: 1;
    height: 26px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    outline: none;
    background: var(--bg);
    color: var(--text);
    font: 12px var(--mono);
  }
  .find-input:focus {
    border-color: var(--accent);
  }
  .find-togs {
    display: flex;
    gap: 2px;
  }
  .find-tog {
    height: 24px;
    min-width: 26px;
    padding: 0 5px;
    border: 1px solid transparent;
    border-radius: 5px;
    background: none;
    color: var(--text-faint);
    font: 600 11px var(--mono);
    cursor: pointer;
  }
  .find-tog:hover {
    color: var(--text);
    background: var(--bg-hover);
  }
  .find-tog.on {
    color: var(--accent);
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .find-count {
    min-width: 50px;
    color: var(--text-dim);
    font: 11.5px var(--mono);
    text-align: center;
  }
  .find-count.err {
    color: var(--red);
  }
  .find-btn {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font-size: 15px;
    cursor: pointer;
  }
  .find-btn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--text);
  }
  .find-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }
</style>
