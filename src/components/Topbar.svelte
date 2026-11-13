<script>
  import { store } from '../lib/store.svelte.js';
  import Modal from './Modal.svelte';

  // HTTP and WebSocket holds share one queue in the UI, so both count toward "someone
  // is waiting on you". Only meaningful while intercept is on: turning it off releases
  // everything (Interceptor::set_enabled), so a stale queue cannot keep this alive.
  const heldCount = $derived(store.intercept.enabled ? store.intercept.queue.length + store.wsIntercept.queue.length : 0);

  const titles = { history: 'HTTP History', intercept: 'Intercept', repeater: 'Repeater', intruder: 'Intruder', passive: 'Passive Scan', decoder: 'Decoder', logs: 'Logs', help: 'Guide', settings: 'Settings' };

  /** Spell out what the warning means: which listener, and why its reach is a surprise
   *  rather than something the address makes obvious. */
  function exposureTitle() {
    const l = store.exposedListener;
    const where = l ? `${l.host}:${l.port}` : store.proxy.host;
    const reach =
      store.exposure === 'all'
        ? `Listening on ${where}, which is every interface - including any network that connects later.`
        : `Listening on ${where}, an address on a VPN interface, so the far side of the tunnel can reach the proxy.`;
    const auth = store.config.auth.enabled
      ? 'Authentication is on.'
      : 'Anyone who can reach it can use it, with no authentication.';
    return `${reach} ${auth} Click for Settings > Proxy listener.`;
  }

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
    if (action === 'new') await store.newProject(); // name it, then it is saved
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
  <button
    class="btn sm intercept"
    class:active={store.intercept.enabled}
    class:holding={heldCount > 0}
    title={heldCount
      ? `${heldCount} message${heldCount === 1 ? '' : 's'} held in Intercept - the client is waiting. Open Intercept to forward or drop.`
      : 'Hold requests and responses for editing before they go on'}
    onclick={() => store.toggleIntercept()}>
    Intercept {store.intercept.enabled ? 'on' : 'off'}
  </button>
  {#if store.exposure}
    <button
      class="exposed"
      class:danger={!store.config.auth.enabled}
      title={exposureTitle()}
      onclick={() => store.openListenerSettings()}
    >
      ⚠ {store.exposure === 'all' ? 'All interfaces' : 'VPN'}{store.config.auth.enabled ? '' : ' · no auth'}
    </button>
  {/if}
  <button
    class="status"
    class:on={store.proxy.running}
    title={store.proxy.running
      ? `Listening on ${store.proxy.host}:${store.proxy.port}. Open the listener settings.`
      : 'The proxy is not running. Open the listener settings.'}
    onclick={() => store.openListenerSettings()}
  >
    <span class="dot"></span>
    {#if store.proxy.running}
      Listening <span class="mono">{store.proxy.host}:{store.proxy.port}</span>
    {:else}
      Proxy stopped
    {/if}
  </button>
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
  /* A button, not a label: it is the shortest route to the listener settings, which is
     where someone looks the moment they read it. */
  .status {
    display: flex;
    align-items: center;
    gap: 7px;
    height: 26px;
    padding: 0 10px;
    border: 0;
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
  }
  .status:hover {
    background: var(--bg-hover);
    color: var(--text);
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
  /* Traffic is held right now and the client is sitting there waiting. Forgetting
     intercept is on reads as "the app is broken" or "the network is down", so this
     has to be noticeable from across the window, not just legible up close. */
  .intercept.holding {
    border-color: var(--red);
    background: color-mix(in srgb, var(--red) 16%, transparent);
    color: var(--red);
    box-shadow: 0 0 0 2px color-mix(in srgb, var(--red) 28%, transparent);
    animation: held-nudge 1.7s ease-in-out infinite;
  }
  /* A burst, then a rest. A pill that shakes without pause is unbearable to sit next
     to, and the pause is what makes the next burst register. */
  @keyframes held-nudge {
    0%,
    16%,
    100% {
      transform: translateX(0);
    }
    2%,
    8%,
    14% {
      transform: translateX(-2.5px);
    }
    5%,
    11% {
      transform: translateX(2.5px);
    }
  }
  /* Reduced motion keeps the colour and the ring, which carry the signal on their own,
     and drops only the movement. */
  @media (prefers-reduced-motion: reduce) {
    .intercept.holding {
      animation: none;
    }
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
