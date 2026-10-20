<script>
  import { store } from '../lib/store.svelte.js';
  import Modal from './Modal.svelte';

  const titles = { history: 'HTTP History', intercept: 'Intercept', repeater: 'Repeater', intruder: 'Intruder', passive: 'Passive Scan', decoder: 'Decoder', logs: 'Logs', help: 'Guide', settings: 'Settings' };

  function toggle() {
    store.proxy.running ? store.stopProxy() : store.startProxy();
  }

  // Project menu (dropdown) + a "save first?" confirm modal for New/Close.
  let wrap = $state();
  let menuOpen = $state(false);
  let confirm = $state(null); // 'new' | 'close' | null
  let saving = $state(false);

  const menuSave = () => ((menuOpen = false), store.saveProject());
  const menuSaveAs = () => ((menuOpen = false), store.saveProjectAs());
  const menuSettings = () => ((menuOpen = false), (store.view = 'settings'));
  const ask = (action) => ((menuOpen = false), (confirm = action));

  async function proceed() {
    const action = confirm;
    confirm = null;
    if (action === 'new') await store.blankProject();
    else if (action === 'close') await store.closeProject();
  }
  async function saveThenProceed() {
    saving = true;
    const ok = await store.saveProject();
    saving = false;
    if (ok) await proceed(); // if the Save-as dialog was cancelled, keep choosing
  }
</script>

<svelte:window onmousedown={(e) => menuOpen && wrap && !wrap.contains(e.target) && (menuOpen = false)} />

<header class="topbar">
  <h1>{titles[store.view]}</h1>
  {#if store.project.name}
    <div class="proj-wrap" bind:this={wrap}>
      <button class="project" class:open={menuOpen} title={store.project.path} onclick={() => (menuOpen = !menuOpen)}>
        <svg viewBox="0 0 24 24"><path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" /></svg>
        {store.project.name}
        <span class="caret">▾</span>
      </button>
      {#if menuOpen}
        <div class="menu" role="menu">
          <button class="mi" role="menuitem" onclick={menuSave}>Save</button>
          <button class="mi" role="menuitem" onclick={menuSaveAs}>Save as…</button>
          <div class="msep"></div>
          <button class="mi" role="menuitem" onclick={() => ask('close')}>Close project</button>
          <button class="mi" role="menuitem" onclick={() => ask('new')}>New project</button>
          <div class="msep"></div>
          <button class="mi" role="menuitem" onclick={menuSettings}>Project settings</button>
        </div>
      {/if}
    </div>
  {/if}
  <div class="spacer"></div>

  {#if store.notice}
    <span class="notice" class:error={store.notice.error}>{store.notice.text}</span>
  {/if}
  {#if store.proxyError}
    <span class="error" title={store.proxyError}>{store.proxyError}</span>
  {/if}

  <button class="btn sm theme" title="Theme: {store.theme} (click to change)" aria-label="Toggle theme" onclick={() => store.cycleTheme()}>
    {store.theme === 'dark' ? '☾' : store.theme === 'light' ? '☀' : '◐'}
  </button>
  <button class="btn sm intercept" class:active={store.intercept.enabled} onclick={() => store.toggleIntercept()}>
    Intercept {store.intercept.enabled ? 'on' : 'off'}
  </button>
  {#if store.exposed}
    <button
      class="exposed"
      class:danger={!store.config.auth.enabled}
      title="Listener {store.proxy.host} is reachable from the network{store.config.auth.enabled ? '' : ' with no authentication'}. Click for Settings."
      onclick={() => (store.view = 'settings')}
    >
      ⚠ LAN{store.config.auth.enabled ? '' : ' · no auth'}
    </button>
  {/if}
  <div class="status" class:on={store.proxy.running}>
    <span class="dot"></span>
    {#if store.proxy.running}
      Listening <span class="mono">{store.proxy.host}:{store.proxy.port}</span>
    {:else}
      Proxy stopped
    {/if}
  </div>
  <button class="btn sm" onclick={toggle}>{store.proxy.running ? 'Stop' : 'Start'}</button>
</header>

{#if confirm}
  <Modal title={confirm === 'new' ? 'New project' : 'Close project'} onclose={() => !saving && (confirm = null)}>
    <p class="cmsg">
      Save the current project <b>{store.project.name}</b> before {confirm === 'new' ? 'starting a new one' : 'closing'}?
    </p>
    {#if saving}
      <p class="cspin">Compressing and saving… this can take a moment for a large project.</p>
    {/if}
    <div class="cactions">
      <button class="btn" onclick={saveThenProceed} disabled={saving}>{saving ? 'Saving…' : 'Save & continue'}</button>
      <button class="btn ghost" onclick={proceed} disabled={saving}>{confirm === 'new' ? 'New without saving' : 'Close without saving'}</button>
      <button class="btn ghost" onclick={() => (confirm = null)} disabled={saving}>Cancel</button>
    </div>
  </Modal>
{/if}

<style>
  .topbar {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 48px;
    padding: 0 14px 0 18px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elev);
  }
  h1 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }
  .proj-wrap {
    position: relative;
  }
  .project {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 24px;
    padding: 0 9px;
    border: 1px solid var(--border);
    border-radius: 99px;
    background: none;
    color: var(--text-dim);
    font: 12px var(--sans);
    cursor: pointer;
  }
  .project:hover,
  .project.open {
    color: var(--text);
    border-color: var(--accent);
  }
  .project svg {
    width: 13px;
    height: 13px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linejoin: round;
  }
  .caret {
    font-size: 9px;
    opacity: 0.7;
  }
  .menu {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 60;
    min-width: 170px;
    padding: 4px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-elev);
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.35);
  }
  .mi {
    display: block;
    width: 100%;
    padding: 6px 10px;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text);
    font: 12.5px var(--sans);
    text-align: left;
    cursor: pointer;
  }
  .mi:hover {
    background: var(--bg-hover);
  }
  .msep {
    height: 1px;
    margin: 4px 6px;
    background: var(--border);
  }
  .cmsg {
    margin: 0 0 12px;
    font-size: 13px;
  }
  .cspin {
    margin: 0 0 12px;
    color: var(--text-dim);
    font-size: 12px;
  }
  .cactions {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 8px;
  }
  .status {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 26px;
    padding: 0 10px;
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font-size: 12px;
  }
  .status.on {
    color: var(--text);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--text-faint);
  }
  .on .dot {
    background: var(--green);
    box-shadow: 0 0 0 3px rgb(62 207 142 / 0.2);
  }
  .theme {
    width: 30px;
    justify-content: center;
    padding: 0;
    font-size: 14px;
  }
  .intercept.active {
    border-color: var(--amber);
    background: color-mix(in srgb, var(--amber) 14%, transparent);
    color: var(--amber);
  }
  .exposed {
    height: 24px;
    padding: 0 9px;
    border: 1px solid color-mix(in srgb, var(--amber) 50%, transparent);
    border-radius: 99px;
    background: color-mix(in srgb, var(--amber) 12%, transparent);
    color: var(--amber);
    font: 600 11.5px var(--sans);
    cursor: pointer;
  }
  .exposed.danger {
    border-color: color-mix(in srgb, var(--red) 50%, transparent);
    background: color-mix(in srgb, var(--red) 12%, transparent);
    color: var(--red);
  }
  .notice {
    color: var(--green);
    font-size: 12px;
  }
  .error {
    max-width: 380px;
    overflow: hidden;
    color: var(--red);
    font-size: 12px;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
</style>
