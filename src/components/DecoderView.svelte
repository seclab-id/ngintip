<script>
  import { store } from '../lib/store.svelte.js';
  import { OPERATIONS, opById, runRecipe } from '../lib/codec.js';

  const dec = store.decoder;
  const groups = [...new Set(OPERATIONS.map((o) => o.group))];
  let copied = $state(false);

  // Output is the input run through the whole recipe (each step feeds the next).
  let output = $state('');
  let error = $state('');
  let errorStep = $state(-1);
  $effect(() => {
    const input = dec.input;
    const ids = [...dec.recipe]; // track array changes
    let cancelled = false;
    runRecipe(input, ids).then((r) => {
      if (cancelled) return;
      output = r.output;
      error = r.error;
      errorStep = r.errorStep;
      dec.output = r.output; // keep in the store for copy / re-use
    });
    return () => (cancelled = true);
  });

  const addOp = (op) => dec.recipe.push(op.id);
  const removeStep = (i) => dec.recipe.splice(i, 1);
  const clearRecipe = () => (dec.recipe = []);
  function moveStep(i, d) {
    const j = i + d;
    if (j < 0 || j >= dec.recipe.length) return;
    [dec.recipe[i], dec.recipe[j]] = [dec.recipe[j], dec.recipe[i]];
  }
  function useAsInput() {
    dec.input = output;
    dec.recipe = [];
  }

  async function copy() {
    await navigator.clipboard.writeText(output);
    copied = true;
    setTimeout(() => (copied = false), 1200);
  }
</script>

<div class="decoder">
  <div class="panel">
    <header class="panel-head">
      <span class="panel-title">Input</span>
      <span class="dim count">{dec.input.length} chars</span>
      <div class="spacer"></div>
      <button class="btn ghost sm" onclick={() => ((dec.input = ''), (dec.recipe = []))} disabled={!dec.input && !dec.recipe.length}>Clear</button>
    </header>
    <textarea class="code area" bind:value={dec.input} placeholder="Paste text, a token, or encoded data here…" spellcheck="false"></textarea>
  </div>

  <div class="middle">
    <div class="ops">
      {#each groups as g}
        <div class="group">
          <span class="label">{g}</span>
          {#each OPERATIONS.filter((o) => o.group === g) as op}
            <button class="btn sm" title="Add “{op.label}” to the recipe" onclick={() => addOp(op)}>{op.label}</button>
          {/each}
        </div>
      {/each}
    </div>

    <div class="recipe">
      <div class="recipe-head">
        <span class="label">Recipe</span>
        <span class="dim">applied top → bottom; click an operation above to add a step</span>
        <div class="spacer"></div>
        <button class="btn ghost sm" onclick={clearRecipe} disabled={!dec.recipe.length}>Clear recipe</button>
      </div>
      {#if !dec.recipe.length}
        <div class="recipe-empty dim">No steps — the input passes through unchanged. Chain e.g. <span class="mono">URL → Base64 → Hex</span>.</div>
      {:else}
        <ol class="steps">
          {#each dec.recipe as id, i (i + ':' + id)}
            {@const op = opById(id)}
            <li class="step" class:failed={errorStep === i}>
              <span class="num">{i + 1}</span>
              <span class="sgroup">{op?.group}</span>
              <span class="sname">{op?.label ?? id}</span>
              <div class="spacer"></div>
              <button class="mini" title="Move up" aria-label="Move up" onclick={() => moveStep(i, -1)} disabled={i === 0}>↑</button>
              <button class="mini" title="Move down" aria-label="Move down" onclick={() => moveStep(i, 1)} disabled={i === dec.recipe.length - 1}>↓</button>
              <button class="mini del" title="Remove" aria-label="Remove step" onclick={() => removeStep(i)}>×</button>
            </li>
          {/each}
        </ol>
      {/if}
    </div>
  </div>

  <div class="panel">
    <header class="panel-head">
      <span class="panel-title">Output</span>
      {#if error}<span class="error">Step {errorStep + 1}{opById(dec.recipe[errorStep]) ? ` (${opById(dec.recipe[errorStep]).label})` : ''}: {error}</span>{/if}
      <div class="spacer"></div>
      <button class="btn ghost sm" title="Use the output as input and clear the recipe" onclick={useAsInput} disabled={!output}>↑ Use as input</button>
      <button class="btn ghost sm" onclick={copy} disabled={!output}>{copied ? 'Copied ✓' : 'Copy'}</button>
    </header>
    <textarea class="code area" value={error ? '' : output} readonly spellcheck="false"></textarea>
  </div>
</div>

<style>
  .decoder {
    display: grid;
    grid-template-rows: 1fr auto 1fr;
    gap: 10px;
    height: 100%;
    padding: 10px;
  }
  .area {
    flex: 1;
    width: 100%;
    border: 0;
    outline: none;
    resize: none;
    background: transparent;
    color: var(--text);
  }
  .count {
    margin-left: 8px;
    font-size: 11.5px;
  }
  .middle {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .ops {
    display: flex;
    flex-wrap: wrap;
    gap: 8px 20px;
  }
  .group {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
  }
  .label {
    margin-right: 3px;
    color: var(--text-dim);
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .recipe {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-elev);
  }
  .recipe-head {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .recipe-head .dim {
    font-size: 11.5px;
  }
  .recipe-empty {
    font-size: 12px;
  }
  .steps {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .step {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
  }
  .step.failed {
    border-color: var(--red);
    box-shadow: inset 0 0 0 1px var(--red);
  }
  .num {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 600 10px var(--sans);
  }
  .sgroup {
    color: var(--text-faint);
    font: 600 9.5px var(--sans);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .sname {
    font: 12.5px var(--sans);
    color: var(--text);
  }
  .mini {
    width: 22px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
  }
  .mini:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--accent);
  }
  .mini:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .mini.del:hover {
    color: var(--red);
    border-color: var(--red);
  }
  .spacer {
    flex: 1;
  }
  .error {
    margin-left: 10px;
    overflow: hidden;
    color: var(--red);
    font-size: 12px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
</style>
