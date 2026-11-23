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
  import OrganizerView from './components/OrganizerView.svelte';
  import PassiveView from './components/PassiveView.svelte';
  import HelpView from './components/HelpView.svelte';
  import LogsView from './components/LogsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import ProjectChooser from './components/ProjectChooser.svelte';
  import ConfirmDialog from './components/ConfirmDialog.svelte';
  import NewProjectDialog from './components/NewProjectDialog.svelte';
  import UpdateDialog from './components/UpdateDialog.svelte';
  import { store } from './lib/store.svelte.js';

  onMount(() => store.init());

  // A full page reload in the desktop webview throws away all in-memory state and
  // drops the user back on the project chooser - it looks like a crash. WebView2 on
  // Windows (and other webviews) still honour Ctrl/Cmd+R, Ctrl+Shift+R and F5, and
  // views without their own key handler (e.g. Organizer) let the keystroke through.
  // Swallow the reload shortcuts whenever we're running inside the Tauri app; a plain
  // dev browser at localhost:1420 keeps its normal reload.
  function guardReload(e) {
    if (!('__TAURI_INTERNALS__' in window) && !('__TAURI__' in window)) return;
    const isReload = e.key === 'F5' || ((e.ctrlKey || e.metaKey) && (e.key === 'r' || e.key === 'R'));
    if (isReload) e.preventDefault();
  }

  // Count up while a project is saving/opening, so a long save shows how long it has
  // been running (0.0s and climbing) rather than a static spinner.
  let busy = $derived(store.decompressing || store.saving);
  let elapsed = $state(0);
  // Shortest time the overlay stays on screen once it has appeared. Opening a project no
  // longer freezes the window, so a small project can finish fast enough that the overlay
  // is a flash. This holds the ELEMENT only: it never delays the work, never touches
  // `store.decompressing`, and the timer is set to the true duration at completion rather
  // than counting on through the padding. A failed open still clears the flag in its
  // `finally` and still raises its notification; this only keeps the card on screen a
  // fraction of a second longer.
  const MIN_OVERLAY_MS = 350;
  let overlay = $state(false);
  // Which operation the card is describing, captured when it appears. The flags are
  // already false during the hold below, so reading them live would flip the wording
  // from "Saving project" to "Opening project" on the way out.
  let overlaySaving = $state(false);
  let shownAt = 0;
  $effect(() => {
    if (busy) {
      overlay = true;
      overlaySaving = store.saving;
      shownAt = performance.now();
      elapsed = 0;
      const id = setInterval(() => (elapsed = (performance.now() - shownAt) / 1000), 100);
      return () => clearInterval(id);
    }
    if (!overlay) return;
    // The work just finished. Show the real duration, then hold for what is left of the
    // minimum (which is usually nothing at all).
    elapsed = (performance.now() - shownAt) / 1000;
    const left = MIN_OVERLAY_MS - (performance.now() - shownAt);
    if (left <= 0) return void (overlay = false);
    const id = setTimeout(() => (overlay = false), left);
    return () => clearTimeout(id);
  });
</script>

<svelte:window onkeydown={guardReload} />

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
      {:else if store.view === 'organizer'}
        <OrganizerView />
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

{#if store.newProjectDialog}
  <NewProjectDialog dialog={store.newProjectDialog} />
{/if}

{#if store.confirmDialog}
  <ConfirmDialog dialog={store.confirmDialog} />
{/if}

<!-- Held back while the project chooser owns the screen: the first thing a user sees
     should be their own projects, not a dialog about ours. -->
{#if store.updatePrompt && !store.chooser}
  <UpdateDialog />
{/if}

{#if overlay}
  <div class="decompress-overlay" role="status" aria-live="polite">
    <div class="decompress-card">
      <div class="decompress-anim" aria-hidden="true">
        <span></span><span></span><span></span><span></span><span></span>
      </div>
      <div class="decompress-text">{overlaySaving ? 'Saving project…' : 'Opening project…'}</div>
      <div class="decompress-sub">{overlaySaving ? `Compressing to the .${BRAND.ext} file` : `Decompressing the .${BRAND.ext} file`}</div>
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
    animation: decompress-bounce 0.6s ease-in-out infinite;
  }
  /* NEGATIVE delays, so every bar is already mid-bounce on the first frame and the wave
     reads immediately. Positive delays up to 0.48s on a 1s cycle meant the last bar sat
     still for half a second, which is most of a fast open: the whole group looked like a
     flash rather than an animation. */
  .decompress-anim span:nth-child(2) { animation-delay: -0.08s; }
  .decompress-anim span:nth-child(3) { animation-delay: -0.16s; }
  .decompress-anim span:nth-child(4) { animation-delay: -0.24s; }
  .decompress-anim span:nth-child(5) { animation-delay: -0.32s; }
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
