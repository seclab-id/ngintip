<script>
  import { tick } from 'svelte';
  import { store } from '../lib/store.svelte.js';
  import MessageView from './MessageView.svelte';
  import SplitPane from './SplitPane.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import Modal from './Modal.svelte';
  import CaptureEditor from './CaptureEditor.svelte';
  import { HIGHLIGHTS } from '../lib/colors.js';
  import { fmtDateTime, statusClass } from '../lib/format.js';
  import { SAVED_COLUMNS, savedRows, notesView, notePreview } from '../lib/organizer.js';

  let tab = $state('collections');
  let menu = $state(null);

  const org = $derived(store.organizer);

  // Focus + select a modal's field once it is on screen.
  function autofocus(node) {
    setTimeout(() => {
      node.focus();
      node.select?.();
    });
  }

  // ---- Shared modals: one-line entry (label, rename) and confirm (delete) ----
  // Field values live in their own (never-null) state: binding to a property of the
  // modal object would be read again after the object is cleared on close.
  let entry = $state(null); // { title, placeholder, ok, run(value) }
  let entryValue = $state('');
  function openEntry(opts, value = '') {
    entryValue = value;
    entry = opts;
  }
  function submitEntry() {
    if (!entry) return;
    const { run } = entry;
    const value = entryValue;
    entry = null;
    run(value);
  }

  // ---- Collections ----
  let newCol = $state('');
  let selectedId = $state(null);
  let query = $state('');
  let sortKey = $state('n');
  let sortDir = $state(1);

  const view = $derived(org.collections.find((c) => c.id === org.viewId) ?? org.collections[0] ?? null);
  const rows = $derived(view ? savedRows(view.items, query, sortKey, sortDir) : []);
  const selected = $derived(view?.items.find((it) => it.id === selectedId) ?? null);

  function addCollection() {
    const name = newCol.trim();
    if (!name) return;
    store.addCollection(name);
    newCol = '';
    selectedId = null;
  }
  function openCollection(id) {
    store.viewCollection(id);
    selectedId = null;
  }
  function sortBy(key) {
    if (sortKey === key) sortDir = -sortDir;
    else {
      sortKey = key;
      sortDir = 1;
    }
  }

  function openColMenu(c, x, y) {
    const isActive = c.id === org.activeId;
    menu = {
      x,
      y,
      items: [
        isActive
          ? { label: 'Deactivate', hint: 'stop saving here', run: () => store.setActiveCollection(null) }
          : { label: 'Set as active', hint: 'saves go here', run: () => store.setActiveCollection(c.id) },
        'separator',
        { label: 'Rename…', run: () => openEntry({ title: 'Rename collection', placeholder: 'Collection name', ok: 'Rename', run: (v) => store.renameCollection(c.id, v) }, c.name) },
        { label: 'Delete…', danger: true, run: () => deleteCollection(c) },
      ],
    };
  }
  async function deleteCollection(c) {
    const drop = () => {
      store.removeCollection(c.id);
      selectedId = null;
    };
    if (!c.items.length) return drop();
    const ok = await store.confirm({
      title: `Delete collection "${c.name}"?`,
      message: `Its ${c.items.length} saved request${c.items.length === 1 ? '' : 's'} (with notes and labels) ${c.items.length === 1 ? 'goes' : 'go'} too. This cannot be undone.`,
    });
    if (ok) drop();
  }

  // ---- Saved requests ----
  async function deleteSaved(it) {
    const ok = await store.confirm({
      title: `Delete saved request "${it.name || `${it.method} ${it.path}`}"?`,
      message: 'Its saved request, response, note and label are removed from the collection. This cannot be undone.',
    });
    if (!ok) return;
    store.removeSavedRequest(it.id);
    if (selectedId === it.id) selectedId = null;
  }
  let noteModal = $state(null); // { itemId, heading }
  let noteDraft = $state('');
  function openNote(it) {
    noteDraft = it.note ?? '';
    noteModal = { itemId: it.id, heading: `${it.method} ${it.host}${it.path ?? ''}` };
  }
  function saveNote() {
    if (!noteModal) return;
    store.updateSavedRequest(noteModal.itemId, { note: noteDraft });
    noteModal = null;
  }
  function editLabel(it) {
    openEntry({ title: 'Label', placeholder: 'e.g. IDOR candidate, login, admin API', ok: 'Save', run: (v) => store.updateSavedRequest(it.id, { label: v.trim() }) }, it.label ?? '');
  }

  function openItemMenu(it, x, y) {
    selectedId = it.id;
    const colId = view.id;
    const others = org.collections.filter((c) => c.id !== colId);
    menu = {
      x,
      y,
      items: [
        { label: 'Send to Repeater', run: () => store.sendSavedToRepeater(it) },
        { label: 'Send to Intruder', run: () => store.sendSavedToIntruder(it) },
        'separator',
        { label: it.note ? 'View / edit note…' : 'Add note…', run: () => openNote(it) },
        { label: it.label ? 'Edit label…' : 'Set label…', run: () => editLabel(it) },
        {
          label: 'Highlight',
          swatches: HIGHLIGHTS.map((color) => ({
            color,
            on: it.color === color,
            run: () => store.updateSavedRequest(it.id, { color: it.color === color ? '' : color }),
          })),
        },
        { label: 'Clear highlight', disabled: !it.color, run: () => store.updateSavedRequest(it.id, { color: '' }) },
        'separator',
        { label: 'Capture screenshot', run: () => openCapture(it) },
        { label: 'Copy full URL', run: () => store.copyText(`${it.target}${it.path ?? ''}`, 'URL') },
        // One nested entry instead of a line per collection, so many collections stay compact.
        {
          label: 'Move to',
          submenu: others.map((c) => ({
            label: c.name,
            hint: c.id === org.activeId ? `active · ${c.items.length}` : String(c.items.length),
            run: () => store.moveSavedRequest(it.id, c.id),
          })),
        },
        'separator',
        { label: 'Delete…', danger: true, run: () => deleteSaved(it) },
      ],
    };
  }

  // Draggable divider between the table (top) and the request/response detail (bottom).
  let split = $state(0.5);
  let panes = $state();
  let dragging = false;
  function dragStart(e) {
    dragging = true;
    e.currentTarget.setPointerCapture?.(e.pointerId);
  }
  function dragMove(e) {
    if (!dragging || !panes) return;
    const r = panes.getBoundingClientRect();
    split = Math.min(0.85, Math.max(0.15, (e.clientY - r.top) / r.height));
  }
  function dragEnd(e) {
    dragging = false;
    try {
      e.currentTarget.releasePointerCapture?.(e.pointerId);
    } catch {
      /* ignore */
    }
  }

  // Capture (screenshot + annotate), same editor as History / Repeater. For the open
  // item it mirrors what each pane shows and how far it is scrolled.
  let capture = $state(null);
  let reqRef = $state();
  let resRef = $state();
  function openCapture(it) {
    const live = it.id === selectedId;
    capture = {
      method: it.method,
      host: it.host,
      url: `${it.target}${it.path ?? ''}`,
      status: it.status,
      title: it.label || notePreview(it.note),
      request: it.request,
      response: it.response,
      requestView: (live && reqRef?.displayText?.()) || it.request,
      responseView: (live && resRef?.displayText?.()) || it.response,
      reqFrac: live ? (reqRef?.visibleFraction?.() ?? null) : null,
      resFrac: live ? (resRef?.visibleFraction?.() ?? null) : null,
    };
  }

  // ---- Notes ----
  let noteQuery = $state('');
  let noteId = $state(null);
  let titleEl = $state();
  const shownNotes = $derived(notesView(org.notes, noteQuery));
  const curNote = $derived(org.notes.find((n) => n.id === noteId) ?? null);
  const words = $derived(curNote ? (curNote.body.trim().match(/\S+/g) ?? []).length : 0);
  // Open the newest note when the tab shows, or after the open one is deleted.
  $effect(() => {
    if (tab === 'notes' && !curNote && shownNotes.length) noteId = shownNotes[0].id;
  });
  async function newNote() {
    noteQuery = '';
    noteId = store.addNote();
    await tick();
    titleEl?.focus();
  }
  async function deleteNote(n) {
    if (!n.title.trim() && !n.body.trim()) return store.removeNote(n.id);
    if (await store.confirm({ title: `Delete note "${n.title || 'Untitled'}"?`, message: 'This cannot be undone.' })) store.removeNote(n.id);
  }
  function openNoteMenu(n, x, y) {
    noteId = n.id;
    menu = {
      x,
      y,
      items: [
        { label: 'Duplicate', run: () => (noteId = store.duplicateNote(n.id)) },
        'separator',
        { label: 'Delete…', danger: true, run: () => deleteNote(n) },
      ],
    };
  }
  /** Today: time only; otherwise a short date. */
  function fmtShort(ms) {
    if (!ms) return '';
    const d = new Date(ms);
    const today = new Date();
    return d.toDateString() === today.toDateString()
      ? d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
      : d.toLocaleDateString([], { day: 'numeric', month: 'short', year: d.getFullYear() === today.getFullYear() ? undefined : 'numeric' });
  }
</script>

{#snippet noteIcon()}
  <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" /><path d="M14 3v6h6M8 13h8M8 17h5" /></svg>
{/snippet}

<div class="organizer">
  <header class="head">
    <h1>Organizer</h1>
    <div class="tabs" role="tablist">
      <button class="tab" class:active={tab === 'collections'} role="tab" aria-selected={tab === 'collections'} onclick={() => (tab = 'collections')}>
        Collections
        {#if store.organizerCount}<span class="pill">{store.organizerCount}</span>{/if}
      </button>
      <button class="tab" class:active={tab === 'notes'} role="tab" aria-selected={tab === 'notes'} onclick={() => (tab = 'notes')}>
        Notes
        {#if org.notes.length}<span class="pill">{org.notes.length}</span>{/if}
      </button>
    </div>
  </header>

  {#if tab === 'collections'}
    <div class="cols">
      <aside class="col-list">
        <div class="side-title">Collections</div>
        <div class="col-scroll">
          {#each org.collections as c (c.id)}
            <button
              class="col-item"
              class:viewing={c.id === view?.id}
              title={c.id === org.activeId ? 'Active: "Save to Organizer" goes here. Right-click for options.' : 'Right-click to set active, rename or delete'}
              onclick={() => openCollection(c.id)}
              oncontextmenu={(e) => (e.preventDefault(), openColMenu(c, e.clientX, e.clientY))}
            >
              <span class="adot" class:on={c.id === org.activeId}></span>
              <span class="col-name">{c.name}</span>
              {#if c.id === org.activeId}<span class="atag">active</span>{/if}
              <span class="col-count">{c.items.length}</span>
            </button>
          {:else}
            <p class="side-empty">No collections yet.</p>
          {/each}
        </div>
        <div class="new-col">
          <input class="input sm" placeholder="New collection…" bind:value={newCol} onkeydown={(e) => e.key === 'Enter' && addCollection()} />
          <button class="btn primary sm" onclick={addCollection} disabled={!newCol.trim()}>Add</button>
        </div>
        <p class="side-hint"><span class="adot on"></span> active = where "Save to Organizer" goes. Right-click a collection to change it.</p>
      </aside>

      <section class="col-main">
        {#if view}
          <div class="col-head">
            <span class="col-title" title={view.name}>{view.name}</span>
            {#if view.id === org.activeId}<span class="atag">active</span>{/if}
            <span class="dim count">{query ? `${rows.length} of ${view.items.length}` : `${view.items.length} saved`}</span>
            <span class="grow"></span>
            <label class="search">
              <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg>
              <input placeholder="Search host, path, label, note…" bind:value={query} spellcheck="false" aria-label="Search saved requests" />
              {#if query}<button class="clear" aria-label="Clear search" onclick={() => (query = '')}>×</button>{/if}
            </label>
          </div>

          <div class="panes" bind:this={panes} style="grid-template-rows: {selected ? `${split}fr 5px ${1 - split}fr` : '1fr'}">
            <div class="pane table-pane">
              <div class="rtable" role="table">
                <div class="rrow rhead" role="row">
                  {#each SAVED_COLUMNS as col (col.key)}
                    <button class="th" class:sorted={sortKey === col.key} onclick={() => sortBy(col.key)} title="Sort by {col.label}">
                      {col.label}{#if sortKey === col.key}<span class="arrow">{sortDir > 0 ? '↑' : '↓'}</span>{/if}
                    </button>
                  {/each}
                  <span class="th plain">Note</span>
                </div>
                <div class="rbody">
                  {#each rows as { it, n } (it.id)}
                    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                    <div
                      class="rrow {it.color ? `c-${it.color}` : ''}"
                      class:marked={it.color}
                      class:sel={it.id === selectedId}
                      role="row"
                      tabindex="-1"
                      onclick={() => (selectedId = it.id)}
                      oncontextmenu={(e) => (e.preventDefault(), openItemMenu(it, e.clientX, e.clientY))}
                    >
                      <span class="cell no">{n}</span>
                      <span class="cell"><span class="src src-{it.source}">{it.source || '-'}</span></span>
                      <span class="cell method m-{it.method?.toLowerCase()}">{it.method}</span>
                      <span class="cell host" title={it.host}>{it.host}</span>
                      <span class="cell path" title={it.path}>{it.path}</span>
                      <span class="cell status {it.status ? statusClass(it.status) : ''}">{it.status ?? ''}</span>
                      <span class="cell label" title={it.label}>{it.label}</span>
                      <span class="cell notec">
                        <button class="note-btn" class:has={!!it.note} title={it.note ? notePreview(it.note) : 'Add a note'} aria-label="Note" onclick={(e) => (e.stopPropagation(), openNote(it))}>
                          {@render noteIcon()}
                        </button>
                      </span>
                    </div>
                  {:else}
                    <p class="empty">
                      {#if query}No saved request matches "{query}".{:else}This collection is empty. Right-click a request in History, Repeater or Intruder and choose "Save to Organizer".{/if}
                    </p>
                  {/each}
                </div>
              </div>
            </div>

            {#if selected}
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="divider" onpointerdown={dragStart} onpointermove={dragMove} onpointerup={dragEnd}></div>
              <div class="pane detail">
                <div class="dhead">
                  <span class="method m-{selected.method?.toLowerCase()}">{selected.method}</span>
                  <span class="durl mono" title="{selected.target}{selected.path ?? ''}">{selected.target}{selected.path ?? ''}</span>
                  {#if selected.status}<span class="dstatus {statusClass(selected.status)}">{selected.status}</span>{/if}
                  {#if selected.source}<span class="src src-{selected.source}">{selected.source}</span>{/if}
                  {#if selected.label}<span class="dlabel" title={selected.label}>{selected.label}</span>{/if}
                  <span class="grow"></span>
                  <span class="dim dtime" title="Saved">{fmtDateTime(selected.savedAt)}</span>
                  <button class="icbtn" class:has={!!selected.note} title={selected.note ? 'View / edit note' : 'Add a note'} aria-label="Note" onclick={() => openNote(selected)}>{@render noteIcon()}</button>
                  <button class="icbtn" title="Capture: screenshot the request/response to annotate" aria-label="Capture screenshot" onclick={() => openCapture(selected)}>
                    <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M23 19a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4l2-3h6l2 3h4a2 2 0 0 1 2 2z" /><circle cx="12" cy="13" r="4" /></svg>
                  </button>
                  <button class="icbtn close" title="Close detail" aria-label="Close detail" onclick={() => (selectedId = null)}>×</button>
                </div>
                <div class="msgs">
                  <SplitPane storeKey="organizer" left={reqPane} right={resPane} />
                  {#snippet reqPane()}
                    <MessageView bind:this={reqRef} title="Request" raw={selected.request} placeholder="No request saved" preferPretty />
                  {/snippet}
                  {#snippet resPane()}
                    <MessageView bind:this={resRef} title="Response" raw={selected.response} placeholder="No response saved" preferPretty />
                  {/snippet}
                </div>
              </div>
            {/if}
          </div>
        {:else}
          <div class="blank">
            <p>No collection yet.</p>
            <p class="dim">Create one on the left, then right-click a request in History, Repeater or Intruder and choose "Save to Organizer".</p>
          </div>
        {/if}
      </section>
    </div>
  {:else}
    <div class="notes-app">
      <aside class="nlist">
        <div class="nlist-head">
          <label class="search">
            <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg>
            <input placeholder="Search notes…" bind:value={noteQuery} spellcheck="false" aria-label="Search notes" />
            {#if noteQuery}<button class="clear" aria-label="Clear search" onclick={() => (noteQuery = '')}>×</button>{/if}
          </label>
          <button class="btn primary sm" onclick={newNote}>+ New note</button>
        </div>
        <div class="nlist-scroll">
          {#each shownNotes as n (n.id)}
            <button class="nitem" class:sel={n.id === noteId} onclick={() => (noteId = n.id)} oncontextmenu={(e) => (e.preventDefault(), openNoteMenu(n, e.clientX, e.clientY))}>
              <span class="ntitle">{n.title.trim() || 'Untitled'}</span>
              <span class="nline">
                <span class="ndate">{fmtShort(n.updatedAt)}</span>
                <span class="npreview">{notePreview(n.body) || 'No additional text'}</span>
              </span>
            </button>
          {:else}
            <p class="nempty">{noteQuery ? 'No notes match your search.' : 'No notes yet.'}</p>
          {/each}
        </div>
        <div class="nlist-foot dim">{org.notes.length} note{org.notes.length === 1 ? '' : 's'} · saved with the project</div>
      </aside>

      <section class="neditor">
        {#if curNote}
          <div class="ne-bar">
            <span class="dim">Edited {fmtDateTime(curNote.updatedAt)}</span>
            <span class="grow"></span>
            <span class="dim">{words} word{words === 1 ? '' : 's'} · {curNote.body.length} chars</span>
            <button class="icbtn" title="Duplicate note" aria-label="Duplicate note" onclick={() => (noteId = store.duplicateNote(curNote.id))}>
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
            </button>
            <button class="icbtn danger" title="Delete note" aria-label="Delete note" onclick={() => deleteNote(curNote)}>
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" /></svg>
            </button>
          </div>
          <input
            bind:this={titleEl}
            class="ne-title"
            placeholder="Untitled"
            value={curNote.title}
            spellcheck="false"
            aria-label="Note title"
            oninput={(e) => store.updateNote(curNote.id, { title: e.currentTarget.value })}
          />
          <textarea
            class="ne-body"
            placeholder="Start writing: findings, payloads, credentials to try, to-dos…"
            value={curNote.body}
            spellcheck="false"
            aria-label="Note body"
            oninput={(e) => store.updateNote(curNote.id, { body: e.currentTarget.value })}
          ></textarea>
        {:else}
          <div class="ne-empty">
            <div class="ne-empty-icon">{@render noteIcon()}</div>
            <h3>{org.notes.length ? 'Select a note' : 'No notes yet'}</h3>
            <p class="dim">Keep findings, payloads and to-dos alongside the project. Notes save as you type.</p>
            <button class="btn primary" onclick={newNote}>Create a note</button>
          </div>
        {/if}
      </section>
    </div>
  {/if}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if capture}
  <CaptureEditor source={capture} onclose={() => (capture = null)} />
{/if}

{#if noteModal}
  <Modal title="Note" draggable onclose={() => (noteModal = null)}>
    <div class="note-modal">
      <div class="nm-for mono" title={noteModal.heading}>{noteModal.heading}</div>
      <textarea
        class="input mono note-area"
        bind:value={noteDraft}
        placeholder="Findings, payloads, reminders…"
        spellcheck="false"
        use:autofocus
        onkeydown={(e) => (e.metaKey || e.ctrlKey) && e.key === 'Enter' && saveNote()}
      ></textarea>
      <div class="modal-actions">
        <span class="dim">{noteDraft.length} chars · ⌘↵ to save</span>
        <span class="grow"></span>
        <button class="btn ghost sm" onclick={() => (noteModal = null)}>Cancel</button>
        <button class="btn primary sm" onclick={saveNote}>Save note</button>
      </div>
    </div>
  </Modal>
{/if}

{#if entry}
  <Modal title={entry.title} onclose={() => (entry = null)}>
    <form class="entry" onsubmit={(e) => (e.preventDefault(), submitEntry())}>
      <input class="input" bind:value={entryValue} placeholder={entry.placeholder} spellcheck="false" use:autofocus />
      <div class="modal-actions">
        <span class="grow"></span>
        <button class="btn ghost sm" type="button" onclick={() => (entry = null)}>Cancel</button>
        <button class="btn primary sm" type="submit">{entry.ok}</button>
      </div>
    </form>
  </Modal>
{/if}


<style>
  .organizer {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    padding: 12px 14px;
    gap: 10px;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 16px;
  }
  h1 {
    margin: 0;
    font-size: 15px;
    font-weight: 650;
  }
  .tabs {
    display: flex;
    gap: 2px;
    padding: 2px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-elev);
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 12px;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text-dim);
    font: 500 12.5px var(--sans);
    cursor: pointer;
  }
  .tab:hover {
    color: var(--text);
  }
  .tab.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .pill {
    min-width: 16px;
    padding: 0 5px;
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 700 10px/16px var(--sans);
    text-align: center;
  }
  .tab.active .pill {
    background: var(--accent);
    color: #fff;
  }
  .grow {
    flex: 1;
  }

  /* ---------- Collections ---------- */
  .cols {
    display: grid;
    grid-template-columns: 220px 1fr;
    gap: 10px;
    min-height: 0;
    flex: 1;
  }
  .col-list,
  .col-main,
  .notes-app {
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
    overflow: hidden;
  }
  .col-list {
    display: flex;
    flex-direction: column;
  }
  .side-title {
    padding: 10px 12px 4px;
    color: var(--text-faint);
    font: 700 10.5px var(--sans);
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .col-scroll {
    flex: 1;
    overflow-y: auto;
    padding: 2px 6px 6px;
  }
  .col-item {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    padding: 7px 8px;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text);
    font: 12.5px var(--sans);
    text-align: left;
    cursor: pointer;
  }
  .col-item:hover {
    background: var(--bg-hover);
  }
  .col-item.viewing {
    background: var(--accent-soft);
  }
  .col-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .col-item.viewing .col-name {
    color: var(--accent);
    font-weight: 600;
  }
  .col-count {
    flex: none;
    min-width: 18px;
    padding: 0 5px;
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 700 10px/16px var(--sans);
    text-align: center;
  }
  .adot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    border: 1.5px solid var(--text-faint);
  }
  .adot.on {
    border-color: var(--green);
    background: var(--green);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--green) 22%, transparent);
  }
  .atag {
    flex: none;
    padding: 0 6px;
    border-radius: 99px;
    background: color-mix(in srgb, var(--green) 16%, transparent);
    color: var(--green);
    font: 700 9.5px/16px var(--sans);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .side-empty {
    margin: 6px 8px;
    color: var(--text-faint);
    font-size: 12px;
  }
  .new-col {
    display: flex;
    gap: 6px;
    padding: 8px;
    border-top: 1px solid var(--border);
  }
  .new-col .input {
    flex: 1;
    min-width: 0;
  }
  .side-hint {
    display: flex;
    align-items: baseline;
    gap: 6px;
    margin: 0;
    padding: 0 10px 10px;
    color: var(--text-faint);
    font-size: 11px;
    line-height: 1.45;
  }
  .side-hint .adot {
    transform: translateY(1px);
  }

  .col-main {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .col-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px 8px 12px;
    border-bottom: 1px solid var(--border);
  }
  .col-title {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 650;
  }
  .count {
    flex: none;
    font-size: 12px;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 6px;
    width: min(280px, 45%);
    height: 28px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    color: var(--text-faint);
  }
  .search:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: none;
    color: var(--text);
    font: 12.5px var(--sans);
  }
  .search .clear {
    border: 0;
    background: none;
    color: var(--text-dim);
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
  }

  /* Table (top) / detail (bottom), resizable with the divider. */
  .panes {
    display: grid;
    min-height: 0;
    flex: 1;
  }
  .pane {
    min-height: 0;
    overflow: auto;
  }
  .divider {
    background: var(--border);
    cursor: row-resize;
  }
  .divider:hover {
    background: var(--accent);
  }

  .rtable {
    font: 12.5px var(--sans);
  }
  .rrow {
    display: grid;
    grid-template-columns: 44px 82px 64px minmax(110px, 1fr) minmax(0, 1.7fr) 56px minmax(0, 0.9fr) 46px;
    align-items: center;
    min-height: 28px;
    padding: 0 6px;
    border-bottom: 1px solid var(--border);
    cursor: default;
  }
  .rbody .rrow:nth-child(even) {
    background: var(--row-alt);
  }
  .rbody .rrow:hover {
    background: var(--bg-hover);
  }
  .rbody .rrow.sel {
    background: var(--accent-soft);
  }
  .rrow.marked {
    background: color-mix(in srgb, var(--hl) 16%, transparent) !important;
    box-shadow: inset 3px 0 0 var(--hl);
  }
  .rrow.marked.sel {
    background: color-mix(in srgb, var(--hl) 28%, transparent) !important;
  }
  .rhead {
    position: sticky;
    top: 0;
    z-index: 1;
    min-height: 30px;
    background: var(--bg-elev);
  }
  .th {
    display: flex;
    align-items: center;
    gap: 3px;
    height: 30px;
    padding: 0 6px;
    border: 0;
    background: none;
    color: var(--text-dim);
    font: 600 11.5px var(--sans);
    text-align: left;
    cursor: pointer;
  }
  .th:hover,
  .th.sorted {
    color: var(--text);
  }
  .th.plain {
    cursor: default;
  }
  .arrow {
    color: var(--accent);
  }
  .cell {
    overflow: hidden;
    padding: 0 6px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cell.no {
    color: var(--text-faint);
    font: 11.5px var(--mono);
  }
  .cell.path {
    color: var(--text-dim);
    font: 12px var(--mono);
  }
  .cell.status {
    font: 600 11.5px var(--mono);
  }
  .cell.label {
    color: var(--text);
  }
  .src {
    padding: 1px 7px;
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 600 10.5px var(--sans);
  }
  .src-history {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent);
  }
  .src-repeater {
    background: color-mix(in srgb, var(--blue) 16%, transparent);
    color: var(--blue);
  }
  .src-intruder {
    background: color-mix(in srgb, var(--amber) 16%, transparent);
    color: var(--amber);
  }
  .note-btn {
    display: grid;
    place-items: center;
    width: 24px;
    height: 22px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-faint);
    cursor: pointer;
  }
  .note-btn:hover {
    background: var(--bg-soft);
    color: var(--text);
  }
  .note-btn.has {
    color: var(--accent);
  }
  .empty {
    margin: 0;
    padding: 16px 14px;
    color: var(--text-faint);
    font-size: 12.5px;
    line-height: 1.5;
  }
  .blank {
    padding: 28px;
  }
  .blank p {
    margin: 0 0 6px;
  }

  .detail {
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .dhead {
    display: flex;
    flex: none;
    align-items: center;
    gap: 8px;
    padding: 6px 8px 6px 12px;
    border-bottom: 1px solid var(--border);
  }
  .durl {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .dstatus {
    flex: none;
    font: 600 12px var(--mono);
  }
  .dlabel {
    max-width: 180px;
    overflow: hidden;
    padding: 1px 8px;
    border-radius: 99px;
    background: var(--bg-soft);
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11.5px;
  }
  .dtime {
    flex: none;
    font-size: 11.5px;
  }
  .icbtn {
    display: grid;
    flex: none;
    place-items: center;
    width: 26px;
    height: 26px;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text-dim);
    cursor: pointer;
  }
  .icbtn:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .icbtn.has {
    color: var(--accent);
  }
  .icbtn.close {
    font-size: 17px;
  }
  .icbtn.danger:hover {
    color: var(--red);
  }
  .msgs {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  /* ---------- Notes ---------- */
  .notes-app {
    display: grid;
    grid-template-columns: 270px 1fr;
    flex: 1;
  }
  .nlist {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--border);
  }
  .nlist-head {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px;
    border-bottom: 1px solid var(--border);
  }
  .nlist-head .search {
    width: 100%;
  }
  .nlist-scroll {
    flex: 1;
    overflow-y: auto;
    padding: 6px;
  }
  .nitem {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    padding: 9px 10px;
    border: 0;
    border-radius: 8px;
    background: none;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .nitem:hover {
    background: var(--bg-hover);
  }
  .nitem.sel {
    background: var(--accent-soft);
  }
  .ntitle {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 600 13px var(--sans);
  }
  .nline {
    display: flex;
    gap: 8px;
    min-width: 0;
    font-size: 11.5px;
  }
  .ndate {
    flex: none;
    color: var(--text-dim);
  }
  .npreview {
    overflow: hidden;
    color: var(--text-faint);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .nempty {
    margin: 12px 8px;
    color: var(--text-faint);
    font-size: 12px;
  }
  .nlist-foot {
    padding: 8px 12px;
    border-top: 1px solid var(--border);
    font-size: 11px;
  }
  .neditor {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .ne-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px 8px 22px;
    border-bottom: 1px solid var(--border);
    font-size: 11.5px;
  }
  .ne-title {
    margin: 18px 22px 6px;
    padding: 0;
    border: 0;
    outline: 0;
    background: none;
    color: var(--text);
    font: 700 20px/1.3 var(--sans);
  }
  .ne-title::placeholder,
  .ne-body::placeholder {
    color: var(--text-faint);
  }
  .ne-body {
    flex: 1;
    min-height: 0;
    margin: 0;
    padding: 6px 22px 22px;
    border: 0;
    outline: 0;
    background: none;
    color: var(--text);
    font: 13.5px/1.65 var(--sans);
    resize: none;
  }
  .ne-empty {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 24px;
    text-align: center;
  }
  .ne-empty h3 {
    margin: 4px 0 0;
    font-size: 15px;
  }
  .ne-empty p {
    max-width: 320px;
    margin: 0 0 8px;
    font-size: 12.5px;
    line-height: 1.5;
  }
  .ne-empty-icon {
    display: grid;
    place-items: center;
    width: 46px;
    height: 46px;
    border-radius: 12px;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .ne-empty-icon :global(svg) {
    width: 22px;
    height: 22px;
  }

  /* ---------- Modals ---------- */
  .note-modal,
  .entry {
    display: flex;
    flex-direction: column;
    gap: 10px;
    width: min(560px, 80vw);
  }
  .nm-for {
    overflow: hidden;
    color: var(--text-dim);
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .note-area {
    min-height: 220px;
    resize: vertical;
    line-height: 1.55;
  }
  .modal-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 11.5px;
  }
</style>
