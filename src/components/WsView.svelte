<script>
  // Messages of a WebSocket flow (status 101), live while the socket is open.
  import { store } from '../lib/store.svelte.js';
  import * as api from '../lib/api.js';
  import { fmtSize } from '../lib/format.js';
  import { highlightBody } from '../lib/highlight.js';
  import { softWrapLong } from '../lib/message.js';
  import MessageView from './MessageView.svelte';

  let { flowId, handshake } = $props();

  const OPCODES = { 0: 'cont', 1: 'text', 2: 'binary', 8: 'close', 9: 'ping', 10: 'pong' };
  // Rendering is capped; the project file keeps everything.
  const SHOWN = 5000;

  let messages = $state.raw([]);
  let selectedId = $state(null);
  let tab = $state('messages');
  let hideControl = $state(false);

  $effect(() => {
    const id = flowId;
    let stale = false;
    messages = [];
    selectedId = null;
    // Subscribe first so nothing is missed between the fetch and the event stream.
    let loaded = false;
    const live = [];
    const off = store.onWsMessage((m) => {
      if (stale || m.flowId !== id) return;
      if (loaded) messages = [...messages, m];
      else live.push(m);
    });
    api.wsMessages(id).then((saved) => {
      if (stale) return;
      const seen = new Set(saved.map((m) => m.id));
      messages = saved.concat(live.filter((m) => !seen.has(m.id)));
      loaded = true;
    });
    return () => {
      stale = true;
      off();
    };
  });

  let visible = $derived.by(() => {
    const all = hideControl ? messages.filter((m) => m.opcode < 8) : messages;
    return all.length > SHOWN ? all.slice(-SHOWN) : all;
  });
  let selected = $derived(messages.find((m) => m.id === selectedId));
  let pretty = $derived.by(() => {
    if (!selected || selected.opcode !== 1) return null;
    try {
      return JSON.stringify(JSON.parse(selected.text), null, 2);
    } catch {
      return null;
    }
  });

  function time(ts) {
    const d = new Date(ts);
    return `${d.toLocaleTimeString([], { hour12: false })}.${String(d.getMilliseconds()).padStart(3, '0')}`;
  }

  function preview(m) {
    return m.text.length > 240 ? `${m.text.slice(0, 240)}…` : m.text;
  }

  function onKey(e) {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
    e.preventDefault();
    const i = visible.findIndex((m) => m.id === selectedId);
    const next = visible[Math.min(visible.length - 1, Math.max(0, i + (e.key === 'ArrowDown' ? 1 : -1)))];
    if (next) selectedId = next.id;
  }

  let copied = $state(false);
  async function copy() {
    await navigator.clipboard.writeText(pretty ?? selected.text);
    copied = true;
    setTimeout(() => (copied = false), 1200);
  }
</script>

{#if tab === 'handshake'}
  <MessageView title="Handshake" raw={handshake}>
    {#snippet actions()}
      <button class="btn ghost sm" onclick={() => (tab = 'messages')}>Messages ({messages.length})</button>
    {/snippet}
  </MessageView>
{:else}
  <div class="panel ws">
    <header class="panel-head">
      <span class="panel-title">WebSocket</span>
      <span class="dim count">{messages.length} messages</span>
      <div class="spacer"></div>
      <label class="toggle"><input type="checkbox" bind:checked={hideControl} /> Hide ping/pong</label>
      <button class="btn ghost sm" onclick={() => (tab = 'handshake')}>Handshake</button>
    </header>

    <div class="list" role="listbox" aria-label="WebSocket messages" tabindex="0" onkeydown={onKey}>
      {#if !messages.length}
        <div class="empty">No messages yet.</div>
      {/if}
      {#if messages.length > visible.length && !hideControl}
        <div class="more dim">Showing the last {SHOWN} messages.</div>
      {/if}
      {#each visible as m (m.id)}
        <button
          class="msg"
          class:selected={m.id === selectedId}
          class:control={m.opcode >= 8}
          role="option"
          aria-selected={m.id === selectedId}
          onclick={() => (selectedId = m.id)}
        >
          <span class="dir {m.direction}" title={m.direction === 'client' ? 'Client → server' : 'Server → client'}>
            {m.direction === 'client' ? '↑' : '↓'}
          </span>
          <span class="dim mono">{time(m.ts)}</span>
          <span class="op mono">{OPCODES[m.opcode] ?? m.opcode}</span>
          <span class="dim mono size">{fmtSize(m.length)}</span>
          <span class="mono text">{preview(m)}</span>
        </button>
      {/each}
    </div>

    {#if selected}
      <div class="detail">
        <div class="detail-head">
          <span class="dim">
            {selected.direction === 'client' ? 'Client → server' : 'Server → client'} · {OPCODES[selected.opcode] ?? selected.opcode}
            · {fmtSize(selected.length)}{selected.length > 1048576 ? ' (disimpan 1 MB pertama)' : ''}
          </span>
          <div class="spacer"></div>
          <button class="btn ghost sm" onclick={copy}>{copied ? 'Copied ✓' : 'Copy'}</button>
        </div>
        <pre class="code">{@html highlightBody(softWrapLong(pretty ?? selected.text))}</pre>
      </div>
    {/if}
  </div>
{/if}

<style>
  .ws {
    display: grid;
    grid-template-rows: auto 1fr auto;
  }
  .count {
    margin-left: 8px;
    font-size: 11.5px;
  }
  .toggle {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-right: 6px;
    color: var(--text-dim);
    font-size: 11.5px;
    cursor: pointer;
  }
  .toggle input {
    accent-color: var(--accent);
  }
  .list {
    min-height: 0;
    overflow: auto;
    outline: none;
  }
  .msg {
    display: grid;
    grid-template-columns: 16px 92px 52px 64px 1fr;
    gap: 8px;
    align-items: center;
    width: 100%;
    height: 26px;
    padding: 0 10px;
    border: 0;
    background: none;
    color: var(--text);
    font: 12px var(--sans);
    text-align: left;
    cursor: default;
  }
  .msg:nth-child(even) {
    background: var(--row-alt);
  }
  .msg:hover {
    background: var(--bg-hover);
  }
  .msg.selected {
    background: var(--accent-soft);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .msg.control .text {
    color: var(--text-dim);
  }
  .dir {
    font-weight: 700;
  }
  .dir.client {
    color: var(--green);
  }
  .dir.server {
    color: var(--blue);
  }
  .op {
    color: var(--text-dim);
    font-size: 11px;
  }
  .size {
    font-size: 11px;
  }
  .text {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .detail {
    display: grid;
    grid-template-rows: auto 1fr;
    height: 45%;
    min-height: 120px;
    max-height: 60vh;
    border-top: 1px solid var(--border);
  }
  .detail-head {
    display: flex;
    align-items: center;
    height: 32px;
    padding: 0 6px 0 12px;
    font-size: 11.5px;
  }
  pre {
    margin: 0;
    overflow: auto;
  }
  .empty,
  .more {
    padding: 14px;
    color: var(--text-faint);
    font-size: 12.5px;
    text-align: center;
  }
  .more {
    padding: 6px;
    font-size: 11.5px;
  }
</style>
