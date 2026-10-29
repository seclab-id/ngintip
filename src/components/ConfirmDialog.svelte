<script>
  // The app-wide Yes/No warning before anything destructive. Opened with
  // `await store.confirm({ title, message, yes, no })`, which resolves true only on Yes.
  // "No" starts focused, so a reflexive Enter, Escape or a click outside keeps the data.
  import { onMount } from 'svelte';
  import { store } from '../lib/store.svelte.js';

  let { dialog } = $props();
  let noBtn = $state();
  let yesBtn = $state();
  // Whatever had the keyboard when the dialog opened (e.g. the History table).
  const opener = typeof document !== 'undefined' ? document.activeElement : null;

  onMount(() => {
    noBtn?.focus();
    // Hand the keyboard back on close, so arrows and Delete keep working in the table.
    return () => {
      if (opener?.isConnected) opener.focus?.();
    };
  });

  // Capture phase on the window: while the dialog is open nothing behind it reacts to a
  // key (no second delete, no Escape closing the modal underneath). Enter/Space still
  // press the focused button; Tab moves between the two buttons only.
  function onKey(e) {
    e.stopPropagation();
    if (e.key === 'Escape') {
      e.preventDefault();
      store.answerConfirm(false);
    } else if (e.key === 'Tab') {
      e.preventDefault();
      (document.activeElement === noBtn ? yesBtn : noBtn)?.focus();
    }
  }
</script>

<svelte:window onkeydowncapture={onKey} />

<div class="backdrop" role="presentation" onmousedown={(e) => e.target === e.currentTarget && store.answerConfirm(false)}>
  <div class="dialog" class:danger={dialog.danger} role="alertdialog" aria-modal="true" aria-labelledby="confirm-title" aria-describedby="confirm-message">
    <div class="icon" aria-hidden="true">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M10.3 4.2 2.6 17.5a2 2 0 0 0 1.7 3h15.4a2 2 0 0 0 1.7-3L13.7 4.2a2 2 0 0 0-3.4 0z" />
        <path d="M12 9.5v4.2" />
        <path d="M12 17.2h.01" />
      </svg>
    </div>
    <div class="text">
      <h2 id="confirm-title">{dialog.title}</h2>
      {#if dialog.message}<p id="confirm-message">{dialog.message}</p>{/if}
    </div>
    <div class="actions">
      <button class="btn no" bind:this={noBtn} onclick={() => store.answerConfirm(false)}>{dialog.no}</button>
      <button class="btn yes" bind:this={yesBtn} onclick={() => store.answerConfirm(true)}>{dialog.yes}</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    /* Above modals and the snapshot editor (200). */
    z-index: 400;
    display: grid;
    place-items: center;
    padding: 24px;
    background: color-mix(in srgb, black 55%, transparent);
    animation: fade 0.12s ease-out;
  }
  .dialog {
    --tone: var(--amber);
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 14px;
    width: min(440px, 100%);
    padding: 18px 18px 16px;
    border: 1px solid var(--border);
    border-top: 3px solid var(--tone);
    border-radius: 12px;
    background: var(--bg-elev);
    box-shadow: 0 20px 60px rgb(0 0 0 / 0.5);
    animation: pop 0.14s ease-out;
  }
  .dialog.danger {
    --tone: var(--red);
  }
  .icon {
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--tone) 16%, transparent);
    color: var(--tone);
  }
  .icon svg {
    width: 21px;
    height: 21px;
  }
  .text {
    min-width: 0;
    padding-top: 2px;
  }
  h2 {
    margin: 0;
    font: 600 14.5px/1.35 var(--sans);
    color: var(--text);
    overflow-wrap: anywhere;
  }
  p {
    margin: 6px 0 0;
    color: var(--text-dim);
    font-size: 12.5px;
    line-height: 1.5;
  }
  .actions {
    grid-column: 1 / -1;
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 12px;
  }
  .btn {
    min-width: 76px;
    justify-content: center;
  }
  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .yes {
    border-color: var(--tone);
    background: var(--tone);
    color: #fff;
    font-weight: 600;
  }
  .yes:hover:not(:disabled) {
    border-color: var(--tone);
    background: color-mix(in srgb, var(--tone) 85%, #000);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.96);
    }
  }
</style>
