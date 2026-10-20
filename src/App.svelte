<script>
  import { onMount } from 'svelte';
  import { BRAND } from './lib/brand.js';
  import Sidebar from './components/Sidebar.svelte';
  import Topbar from './components/Topbar.svelte';
  import HistoryView from './components/HistoryView.svelte';
  import InterceptView from './components/InterceptView.svelte';
  import RepeaterView from './components/RepeaterView.svelte';
  import DecoderView from './components/DecoderView.svelte';
  import ComparerView from './components/ComparerView.svelte';
  import ExtensionsView from './components/ExtensionsView.svelte';
  import IntruderView from './components/IntruderView.svelte';
  import PassiveView from './components/PassiveView.svelte';
  import HelpView from './components/HelpView.svelte';
  import LogsView from './components/LogsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import ProjectChooser from './components/ProjectChooser.svelte';
  import { store } from './lib/store.svelte.js';

  onMount(() => store.init());

  // Count up while a project is saving/opening, so a long save shows how long it has
  // been running (0.0s and climbing) rather than a static spinner.
  let busy = $derived(store.decompressing || store.saving);
  let elapsed = $state(0);
  $effect(() => {
    if (!busy) {
      elapsed = 0;
      return;
    }
    const start = performance.now();
    elapsed = 0;
    const id = setInterval(() => (elapsed = (performance.now() - start) / 1000), 100);
    return () => clearInterval(id);
  });
</script>

{#if store.chooser}
  <ProjectChooser />
{:else}
<div class="shell">
  <Sidebar />
  <main class="main">
    <Topbar />
    <section class="view">
      {#if store.view === 'history'}
        <HistoryView />
      {:else if store.view === 'intercept'}
        <InterceptView />
      {:else if store.view === 'repeater'}
        <RepeaterView />
      {:else if store.view === 'intruder'}
        <IntruderView />
      {:else if store.view === 'passive'}
        <PassiveView />
      {:else if store.view === 'decoder'}
        <DecoderView />
      {:else if store.view === 'comparer'}
        <ComparerView />
      {:else if store.view === 'extensions'}
        <ExtensionsView />
      {:else if store.view === 'logs'}
        <LogsView />
      {:else if store.view === 'help'}
        <HelpView />
      {:else}
        <SettingsView />
      {/if}
    </section>
  </main>
</div>
{/if}

{#if store.decompressing || store.saving}
  <div class="decompress-overlay" role="status" aria-live="polite">
    <div class="decompress-card">
      <div class="decompress-anim" aria-hidden="true">
        <span></span><span></span><span></span><span></span><span></span>
      </div>
      <div class="decompress-text">{store.saving ? 'Saving project…' : 'Opening project…'}</div>
      <div class="decompress-sub">{store.saving ? `Compressing to the .${BRAND.ext} file` : `Decompressing the .${BRAND.ext} file`}</div>
      <div class="decompress-timer" aria-live="off">{elapsed.toFixed(1)}<span class="unit">s</span></div>
    </div>
  </div>
{/if}

<style>
  .shell {
    display: grid;
    grid-template-columns: 56px 1fr;
    height: 100vh;
  }
  .main {
    display: grid;
    grid-template-rows: auto 1fr;
    min-width: 0;
    min-height: 0;
  }
  .view {
    min-height: 0;
    overflow: hidden;
  }

  .decompress-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--bg, #12141a) 78%, transparent);
    backdrop-filter: blur(2px);
  }
  .decompress-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    padding: 28px 40px;
    border-radius: 12px;
    background: var(--bg-elev, #1b1e26);
    border: 1px solid var(--border, #2a2e38);
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.4);
  }
  .decompress-anim {
    display: flex;
    align-items: flex-end;
    gap: 5px;
    height: 34px;
  }
  .decompress-anim span {
    width: 6px;
    height: 100%;
    border-radius: 3px;
    background: var(--accent, #e8722c);
    animation: decompress-bounce 1s ease-in-out infinite;
  }
  .decompress-anim span:nth-child(2) { animation-delay: 0.12s; }
  .decompress-anim span:nth-child(3) { animation-delay: 0.24s; }
  .decompress-anim span:nth-child(4) { animation-delay: 0.36s; }
  .decompress-anim span:nth-child(5) { animation-delay: 0.48s; }
  @keyframes decompress-bounce {
    0%, 100% { transform: scaleY(0.35); opacity: 0.5; }
    50% { transform: scaleY(1); opacity: 1; }
  }
  .decompress-text {
    font-size: 14px;
    font-weight: 600;
    color: var(--text, #e6e6e6);
  }
  .decompress-sub {
    font-size: 12px;
    color: var(--text-dim, #8b90a0);
  }
  .decompress-timer {
    margin-top: 10px;
    font: 700 22px var(--mono, ui-monospace, monospace);
    letter-spacing: 0.02em;
    color: var(--accent, #8b7bff);
    font-variant-numeric: tabular-nums;
  }
  .decompress-timer .unit {
    margin-left: 2px;
    font-size: 13px;
    font-weight: 600;
    color: var(--text-dim, #8b90a0);
  }
</style>
