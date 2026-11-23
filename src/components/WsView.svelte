<script>
  // Messages of a WebSocket flow (status 101), live while the socket is open.
  import { store } from '../lib/store.svelte.js';
  import * as api from '../lib/api.js';
  import { fmtSize } from '../lib/format.js';
  import { highlightBody } from '../lib/highlight.js';
  import { softWrapLong, prettyJsonText } from '../lib/message.js';
  import { frameToCompose, isWholeFrame } from '../lib/wsframe.js';
  import MessageView from './MessageView.svelte';

  // `flowId` drives the live view: load the stored frames and follow the socket.
  // `saved` is an Organizer record's copy ({ total, items }), which is a finished thing:
  // nothing more will arrive for it, so it neither loads nor subscribes.
  // `onstats` reports the conversation's totals to whoever is showing a status line
  // for it (the Repeater's socket toolbar). `onreuse` turns the detail pane into a
  // Repeater: given a frame, the parent loads it into its composer.
  let { flowId = null, handshake, saved = null, onstats = null, onreuse = null, empty = 'No messages yet.' } = $props();

  const OPCODES = { 0: 'cont', 1: 'text', 2: 'binary', 8: 'close', 9: 'ping', 10: 'pong' };
  // Rendering is capped; the project file keeps everything.
  const SHOWN = 5000;

  let messages = $state.raw([]);
  let selectedId = $state(null);
  let tab = $state('messages');
  let hideControl = $state(false);

  $effect(() => {
    // A live socket is watched at its tail; a saved record is read from the top,
    // because nothing more is coming and the first frame is where it starts. Set here
    // rather than at the declaration so switching sockets follows the new one again.
    follow = !saved;
    if (saved) {
      messages = saved.items ?? [];
      selectedId = null;
      return;
    }
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

  // Totals are counted over everything held, not over what is on screen: the list is
  // capped at SHOWN and `Hide ping/pong` is a filter, and a counter that moved when you
  // ticked a checkbox would be worse than no counter.
  $effect(() => {
    if (!onstats) return;
    let up = 0;
    let down = 0;
    let bytes = 0;
    for (const m of messages) {
      bytes += Number(m.length ?? 0);
      if (m.direction === 'client') up += 1;
      else down += 1;
    }
    onstats({ total: messages.length, up, down, bytes });
  });

  // Following the tail. A live socket writes at the bottom, so a list that does not
  // follow looks frozen while frames pile up out of sight. Scrolling up stops it (you
  // are reading something), returning to the bottom starts it again, and the button
  // says which of those is happening instead of leaving it to be guessed.
  let listEl = $state();
  let follow = $state(true);
  function onScroll() {
    if (!listEl) return;
    follow = listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight < 24;
  }
  function toLatest() {
    follow = true;
    if (listEl) listEl.scrollTop = listEl.scrollHeight;
  }
  $effect(() => {
    messages.length;
    if (!follow || !listEl) return;
    // After the rows are in the DOM, not before.
    queueMicrotask(() => {
      if (follow && listEl) listEl.scrollTop = listEl.scrollHeight;
    });
  });

  let reuse = $derived(onreuse && selected ? frameToCompose(selected) : null);
  let whole = $derived(selected ? isWholeFrame(selected) : true);

  let visible = $derived.by(() => {
    const all = hideControl ? messages.filter((m) => m.opcode < 8) : messages;
    return all.length > SHOWN ? all.slice(-SHOWN) : all;
  });
  let selected = $derived(messages.find((m) => m.id === selectedId));
  // Re-indented, never re-serialized: a captured frame holding `\u0063` or a duplicate
  // key must read the same here as it did on the wire. See prettyJsonText.
  let pretty = $derived(selected && selected.opcode === 1 ? prettyJsonText(selected.text) : null);

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
      {#if !follow && messages.length}
        <button class="btn ghost sm latest" title="Scroll to the newest frame and keep following" onclick={toLatest}>↓ Latest</button>
      {/if}
      <label class="toggle"><input type="checkbox" bind:checked={hideControl} /> Hide ping/pong</label>
      <button class="btn ghost sm" onclick={() => (tab = 'handshake')}>Handshake</button>
    </header>

    <div
      class="list"
      role="listbox"
      aria-label="WebSocket messages"
      tabindex="0"
      bind:this={listEl}
      onkeydown={onKey}
      onscroll={onScroll}
    >
      {#if !messages.length}
        <div class="empty">{empty}</div>
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
          {#if reuse}
            <button
              class="btn ghost sm"
              title={whole
                ? 'Load this frame into the composer, where you can edit it and send it again'
                : 'Only the start of this frame was kept: resending sends what is shown here'}
              onclick={() => onreuse(reuse)}
            >
              ↻ Edit &amp; resend{whole ? '' : ' (start)'}
            </button>
          {/if}
          <button class="btn ghost sm" onclick={copy}>{copied ? 'Copied ✓' : 'Copy'}</button>
        </div>
        <pre class="code">{@html highlightBody(softWrapLong(pretty ?? selected.text))}</pre>
      </div>
    {/if}
  </div>
{/if}

<style>
  /* A flex column, not a grid.
     The detail pane used to be a grid row sized `height: 45%`, and a percentage height
     against an `auto` grid track is indefinite, so it was ignored: the pane grew to fit
     the whole message, overflowed the panel, and `.panel { overflow: hidden }` cut it
     off with no scrollbar anywhere to reach the rest. As a flex column the 45% is a
     flex-basis against the panel's own height, which is definite, so the pane keeps its
     share and the message scrolls inside it. */
  .ws {
    display: flex;
    flex-direction: column;
    min-height: 0;
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
    flex: 1;
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
    display: flex;
    flex-direction: column;
    /* Its share of the panel, never less than a few lines and never more than most of
       the window: a 400 KB frame must not leave the message list with nothing. */
    flex: 0 0 45%;
    min-height: 140px;
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
    flex: 1;
    margin: 0;
    /* A scrollable box has to be allowed to be smaller than its content. */
    min-height: 0;
    overflow: auto;
    padding: 10px 12px;
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
