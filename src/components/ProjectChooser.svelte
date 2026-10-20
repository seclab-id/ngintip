<script>
  // Shown at startup so the user picks a project before seeing History.
  import { store } from '../lib/store.svelte.js';
  import { BRAND } from '../lib/brand.js';
  import Logo from './Logo.svelte';

  let busy = $state(false);
  async function run(fn) {
    if (busy) return;
    busy = true;
    try {
      await fn();
    } finally {
      busy = false;
    }
  }
</script>

<div class="chooser">
  <div class="box">
    <div class="brand">
      <Logo size={40} radius={11} />
      <h1>{BRAND.name}</h1>
    </div>
    <p class="dim">Pick a project to store this session's history, or start a blank one and save it later.</p>

    <div class="options">
      {#if store.project.name}
        <button class="opt primary" onclick={() => run(() => store.chooseContinue())}>
          <span class="opt-title">Continue</span>
          <span class="opt-sub mono">{store.project.name}</span>
        </button>
      {/if}
      <button class="opt" onclick={() => run(() => store.newProject())}>
        <span class="opt-title">New project…</span>
        <span class="opt-sub">Choose where to save a new .{BRAND.ext} file</span>
      </button>
      <button class="opt" onclick={() => run(() => store.openProject())}>
        <span class="opt-title">Open project…</span>
        <span class="opt-sub">Open an existing .{BRAND.ext} file</span>
      </button>
      <button class="opt" onclick={() => run(() => store.blankProject())}>
        <span class="opt-title">Blank project</span>
        <span class="opt-sub">Start empty, Save as later</span>
      </button>
    </div>
  </div>
</div>

<style>
  .chooser {
    display: grid;
    place-items: center;
    height: 100vh;
    background: var(--bg);
  }
  .box {
    width: min(440px, 90vw);
    padding: 28px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 8px;
  }
  /* Logo badge comes from <Logo> (src/components/Logo.svelte). */
  h1 {
    margin: 0;
    font-size: 22px;
  }
  p {
    margin: 0 0 18px;
    font-size: 13px;
    line-height: 1.5;
  }
  .options {
    display: grid;
    gap: 8px;
  }
  .opt {
    display: grid;
    gap: 2px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-soft);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
  }
  .opt:hover {
    border-color: var(--accent);
  }
  .opt.primary {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .opt-title {
    font-size: 13.5px;
    font-weight: 600;
  }
  .opt-sub {
    color: var(--text-dim);
    font-size: 12px;
  }
</style>
