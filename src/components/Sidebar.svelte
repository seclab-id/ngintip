<script>
  import { store } from '../lib/store.svelte.js';
  import Logo from './Logo.svelte';

  const items = [
    { id: 'history', label: 'History', icon: 'M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01' },
    { id: 'intercept', label: 'Intercept', icon: 'M10 15V9M14 15V9M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20z' },
    { id: 'repeater', label: 'Repeater', icon: 'm17 2 4 4-4 4M3 11v-1a4 4 0 0 1 4-4h14M7 22l-4-4 4-4M21 13v1a4 4 0 0 1-4 4H3' },
    { id: 'intruder', label: 'Intruder', icon: 'M12 2v4M12 18v4M2 12h4M18 12h4M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8z' },
    { id: 'passive', label: 'Passive Scan', icon: 'M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM12 8v4M12 16h.01' },
    { id: 'decoder', label: 'Decoder', icon: 'M16 18l6-6-6-6M8 6l-6 6 6 6M14 4l-4 16' },
    { id: 'comparer', label: 'Comparer', icon: 'M9 3H5a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h4M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4M12 2v20' },
    { id: 'extensions', label: 'Extensions', icon: 'M20 7h-3V4a2 2 0 0 0-2-2H9a2 2 0 0 0-2 2v3H4a2 2 0 0 0-2 2v6a2 2 0 0 0 2 2h3v-2a2 2 0 1 1 4 0v2h4a2 2 0 0 0 2-2v-3h2a2 2 0 1 0 0-4z' },
    { id: 'logs', label: 'Logs', icon: 'M4 4h16M4 9h16M4 14h10M4 19h7' },
    { id: 'settings', label: 'Settings', icon: 'M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6' },
    { id: 'help', label: 'Guide', icon: 'M12 2a10 10 0 1 0 0 20 10 10 0 0 0 0-20zM9.1 9a3 3 0 0 1 5.8 1c0 2-3 2.5-3 4M12 17h.01' },
  ];
</script>

<nav class="sidebar">
  <div class="logo-wrap"><Logo size={34} shadow title="Ngintip" /></div>
  {#each items as item}
    <button
      class="nav"
      class:active={store.view === item.id}
      aria-label={item.label}
      onclick={() => (store.view = item.id)}
    >
      <svg viewBox="0 0 24 24"><path d={item.icon} /></svg>
      <span class="tip">{item.label}</span>
      {#if item.id === 'intercept' && store.intercept.queue.length}
        <span class="count">{store.intercept.queue.length}</span>
      {:else if item.id === 'intercept' && store.intercept.enabled}
        <span class="on"></span>
      {:else if item.id === 'logs' && store.logsUnread}
        <span class="count">{store.logsUnread}</span>
      {:else if item.id === 'passive' && store.passiveAlerts}
        <span class="count alert">{store.passiveAlerts}</span>
      {:else if item.id === 'comparer' && store.comparer.items.length}
        <span class="count">{store.comparer.items.length}</span>
      {/if}
    </button>
  {/each}
</nav>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    padding: 12px 0;
    background: var(--bg-elev);
    border-right: 1px solid var(--border);
  }
  .tip {
    position: absolute;
    left: calc(100% + 8px);
    top: 50%;
    z-index: 50;
    padding: 4px 9px;
    transform: translateY(-50%) scale(0.96);
    transform-origin: left center;
    border-radius: 6px;
    background: var(--bg-soft);
    border: 1px solid var(--border);
    color: var(--text);
    font-size: 12px;
    white-space: nowrap;
    pointer-events: none;
    opacity: 0;
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.25);
    transition: opacity 0.1s, transform 0.1s;
  }
  .nav:hover .tip,
  .nav:focus-visible .tip {
    opacity: 1;
    transform: translateY(-50%) scale(1);
  }
  /* Logo badge comes from <Logo> (src/components/Logo.svelte); this only spaces it. */
  .logo-wrap {
    margin-bottom: 12px;
  }
  .nav {
    position: relative;
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    border: 0;
    border-radius: 10px;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }
  .nav:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .nav.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .count {
    position: absolute;
    top: 2px;
    right: 1px;
    min-width: 16px;
    height: 16px;
    padding: 0 4px;
    border-radius: 99px;
    background: var(--amber);
    color: #1b1300;
    font: 700 10px/16px var(--sans);
  }
  .count.alert {
    background: var(--red);
    color: #fff;
  }
  .on {
    position: absolute;
    top: 6px;
    right: 6px;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--amber);
  }
  svg {
    width: 19px;
    height: 19px;
    fill: none;
    stroke: currentColor;
    stroke-width: 2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
