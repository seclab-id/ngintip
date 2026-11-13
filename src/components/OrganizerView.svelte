<script>
  import { tick } from 'svelte';
  import { store } from '../lib/store.svelte.js';
  import MessageView from './MessageView.svelte';
  import MiniDecoder from './MiniDecoder.svelte';
  import SplitPane from './SplitPane.svelte';
  import ContextMenu from './ContextMenu.svelte';
  import Modal from './Modal.svelte';
  import CaptureEditor from './CaptureEditor.svelte';
  import { HIGHLIGHTS } from '../lib/colors.js';
  import { fmtDateTime, statusClass } from '../lib/format.js';
  import { SAVED_COLUMNS, savedRows, notesView, notePreview } from '../lib/organizer.js';
  import { splitMessage } from '../lib/message.js';
  import { toCurl } from '../lib/curl.js';
  import { loadColumnWidths, saveColumnWidths, columnTemplate, clampCol } from '../lib/colwidth.js';
  import MarkdownView from './MarkdownView.svelte';
  import { brandKey } from '../lib/brand.js';

  // Which half of the Organizer is showing. The view is unmounted when you switch to
  // History and back, so this has to be remembered somewhere outside the component or
  // every trip back would land on Collections regardless of where you were. Kept per
  // viewer, like the column widths and the note Write/Preview choice.
  const TAB_KEY = brandKey('organizer.tab');
  const TABS = ['collections', 'notes'];
  let tab = $state(loadTab());
  function loadTab() {
    try {
      const saved = localStorage.getItem(TAB_KEY);
      return TABS.includes(saved) ? saved : 'collections';
    } catch {
      return 'collections';
    }
  }
  function setTab(next) {
    tab = next;
    try {
      localStorage.setItem(TAB_KEY, next);
    } catch {
      /* storage unavailable: the choice just lasts while the view is mounted */
    }
  }
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

  // Highlight-to-decode side bar (same as History / Repeater): selecting decodable
  // text in a pane opens MiniDecoder. Clear it when a different saved request is
  // opened or the detail is closed, so a stale decode never lingers.
  let miniDecode = $state(null);
  $effect(() => {
    selectedId;
    miniDecode = null;
  });

  // Saved requests can carry the extension / original variants captured at save time,
  // so the detail shows the same Wire / Extension / Original switcher the live views do.
  function buildVariants(wire, orig, ext) {
    const list = [{ id: 'final', label: 'Wire', raw: wire ?? '' }];
    if (ext && ext !== wire) list.push({ id: 'decoded', label: 'Extension', raw: ext });
    if (orig && orig !== wire) list.push({ id: 'original', label: 'Original', raw: orig });
    return list.length > 1 ? list : null;
  }

  // A whole Intruder run saved as one record keeps every result, shown as a table like
  // Intruder's own results list; the picked row drives the request/response panes.
  const runRows = $derived(Array.isArray(selected?.results) && selected.results.length ? selected.results : null);
  let runSel = $state(null);
  $effect(() => {
    selectedId;
    runSel = null;
  });
  // Search inside a saved run's table (payload, status, row number or error).
  let runQuery = $state('');
  $effect(() => {
    selectedId;
    runQuery = '';
  });
  const runShown = $derived.by(() => {
    if (!runRows) return null;
    const q = runQuery.trim().toLowerCase();
    if (!q) return runRows;
    return runRows.filter(
      (r) =>
        String(r.n).includes(q) ||
        String(r.status ?? '').includes(q) ||
        (r.payloads ?? []).join(', ').toLowerCase().includes(q) ||
        String(r.error ?? '').toLowerCase().includes(q),
    );
  });
  const runRow = $derived(runRows ? (runRows.find((r) => r.n === runSel) ?? runShown?.[0] ?? runRows[0]) : null);

  // ---- Resizable table columns ----
  // Saved requests: the 7 sortable columns plus Note. Widths are px and remembered.
  const COLS = [...SAVED_COLUMNS, { key: 'note', label: 'Note', plain: true }];
  const SAVED_W = [44, 82, 64, 160, 260, 56, 120, 46];
  const RUN_W = [56, 260, 84, 84, 84];
  let savedW = $state(loadColumnWidths('organizer.saved', SAVED_W));
  let runW = $state(loadColumnWidths('organizer.run', RUN_W));
  const savedCols = $derived(columnTemplate(savedW));
  const runCols = $derived(columnTemplate(runW));

  // Drag a header grip to resize that column; double-click it to reset to the default.
  let colDrag = null;
  const widthsOf = (which) => (which === 'saved' ? savedW : runW);
  const storeKeyOf = (which) => (which === 'saved' ? 'organizer.saved' : 'organizer.run');
  function gripDown(e, which, i) {
    e.preventDefault();
    e.stopPropagation();
    colDrag = { which, i, startX: e.clientX, startW: widthsOf(which)[i] };
    e.currentTarget.setPointerCapture?.(e.pointerId);
  }
  function gripMove(e) {
    if (!colDrag) return;
    widthsOf(colDrag.which)[colDrag.i] = clampCol(colDrag.startW + (e.clientX - colDrag.startX));
  }
  function gripUp(e) {
    if (!colDrag) return;
    saveColumnWidths(storeKeyOf(colDrag.which), widthsOf(colDrag.which));
    try {
      e.currentTarget.releasePointerCapture?.(e.pointerId);
    } catch {
      // the pointer was already released
    }
    colDrag = null;
  }
  function gripReset(which, i) {
    const defaults = which === 'saved' ? SAVED_W : RUN_W;
    widthsOf(which)[i] = defaults[i];
    saveColumnWidths(storeKeyOf(which), widthsOf(which));
  }

  // What the panes show: the picked run row, else the record itself.
  const shownReq = $derived(runRow ? runRow.request ?? '' : selected?.request ?? '');
  const shownRes = $derived(runRow ? runRow.response ?? '' : selected?.response ?? '');
  const src = $derived(runRow ?? selected);
  const reqVariants = $derived(src ? buildVariants(shownReq, src.requestOriginal, src.requestExtension) : null);
  const resVariants = $derived(src ? buildVariants(shownRes, src.responseOriginal, src.responseExtension) : null);

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
    noteModalPreview = false;
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

  // Right-click menus on the saved request's Request / Response panes, matching the
  // ones in Repeater (copy, send to Repeater/Intruder/Decoder/Comparer).
  const itemUrl = (it) => `${it.target}${it.path ?? ''}`;
  const itemTitle = (it) => it.name || it.host;
  function openReqMenu(e) {
    if (!selected) return;
    e.preventDefault();
    const raw = shownReq || '';
    const body = splitMessage(raw).body;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: 'Send to Repeater', run: () => store.sendSavedToRepeater(selected) },
        { label: 'Send to Intruder', run: () => store.sendSavedToIntruder(selected) },
        { label: 'Send to Decoder', run: () => store.sendStringToDecoder(raw) },
        { label: 'Send to Comparer', run: () => store.addToComparer(raw, `${itemTitle(selected)} req`, 'req') },
        'separator',
        { label: 'Copy URL', run: () => store.copyText(itemUrl(selected), 'URL') },
        { label: 'Copy as cURL', run: () => store.copyText(toCurl(raw, selected.target), 'cURL command') },
        { label: 'Copy request body', disabled: !body, run: () => store.copyText(body, 'Request body') },
        { label: 'Copy raw request', run: () => store.copyText(raw, 'Request') },
      ],
    };
  }
  function openResMenu(e) {
    if (!selected) return;
    const raw = shownRes || '';
    if (!raw) return; // nothing saved: leave the native menu
    e.preventDefault();
    const body = splitMessage(raw).body;
    menu = {
      x: e.clientX,
      y: e.clientY,
      items: [
        { label: 'Copy response body', disabled: !body, run: () => store.copyText(body, 'Response body') },
        { label: 'Copy raw response', run: () => store.copyText(raw, 'Response') },
        { label: 'Send response to Decoder', run: () => store.sendStringToDecoder(raw) },
        { label: 'Send response to Comparer', run: () => store.addToComparer(raw, `${itemTitle(selected)} res`, 'res') },
        'separator',
        { label: 'Send to Repeater', run: () => store.sendSavedToRepeater(selected) },
        { label: 'Send to Intruder', run: () => store.sendSavedToIntruder(selected) },
        { label: 'Copy URL', run: () => store.copyText(itemUrl(selected), 'URL') },
      ],
    };
  }

  // Ctrl/Cmd+R sends the selected saved request to Repeater (same convention as
  // History/Repeater). Without this, the keystroke used to fall through to the webview
  // and reload the app back to the project chooser.
  function onKey(e) {
    if (!(e.ctrlKey || e.metaKey) || (e.key !== 'r' && e.key !== 'R')) return;
    if (tab !== 'collections' || !selected) return;
    const el = document.activeElement;
    if (el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable)) return;
    e.preventDefault();
    store.sendSavedToRepeater(selected);
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
  // Notes render markdown in Preview; Write stays a plain textarea so the caret, undo
  // stack and selection behave normally. Remembered so the choice survives a view
  // switch, per viewer only (nothing to sync, nothing to save with the project).
  let notePreviewOn = $state(loadNoteMode());
  function loadNoteMode() {
    try {
      return localStorage.getItem(brandKey('organizer.notePreview')) === '1';
    } catch {
      return false;
    }
  }
  function setNoteMode(on) {
    notePreviewOn = on;
    try {
      localStorage.setItem(brandKey('organizer.notePreview'), on ? '1' : '0');
    } catch {
      /* storage unavailable: the choice just lasts this session */
    }
  }
  // The per-request note modal gets the same switch, but always opens on Write: it is
  // reached by "Add note" far more often than to read one back.
  let noteModalPreview = $state(false);
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
      <button class="tab" class:active={tab === 'collections'} role="tab" aria-selected={tab === 'collections'} onclick={() => setTab('collections')}>
        Collections
        {#if store.organizerCount}<span class="pill">{store.organizerCount}</span>{/if}
      </button>
      <button class="tab" class:active={tab === 'notes'} role="tab" aria-selected={tab === 'notes'} onclick={() => setTab('notes')}>
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
              <div class="rtable" role="table" style="--cols: {savedCols}">
                <div class="rrow rhead" role="row">
                  {#each COLS as col, i (col.key)}
                    <div class="thwrap">
                      {#if col.plain}
                        <span class="th plain">{col.label}</span>
                      {:else}
                        <button class="th" class:sorted={sortKey === col.key} onclick={() => sortBy(col.key)} title="Sort by {col.label}">
                          {col.label}{#if sortKey === col.key}<span class="arrow">{sortDir > 0 ? '↑' : '↓'}</span>{/if}
                        </button>
                      {/if}
                      <!-- svelte-ignore a11y_no_static_element_interactions -->
                      <span
                        class="grip"
                        title="Drag to resize, double-click to reset"
                        onpointerdown={(e) => gripDown(e, 'saved', i)}
                        onpointermove={gripMove}
                        onpointerup={gripUp}
                        ondblclick={() => gripReset('saved', i)}
                      ></span>
                    </div>
                  {/each}
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
                {#if runRows}
                  <!-- A saved Intruder run: browse its results like Intruder's own table. -->
                  <div class="runrows" style="--cols: {runCols}">
                    <div class="runbar">
                      <span class="dim">{runQuery ? `${runShown.length} of ${runRows.length}` : `${runRows.length}`} results</span>
                      <span class="grow"></span>
                      <label class="search runsearch">
                        <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg>
                        <input placeholder="Search payload, status…" bind:value={runQuery} spellcheck="false" aria-label="Search results" />
                        {#if runQuery}<button class="clear" aria-label="Clear search" onclick={() => (runQuery = '')}>×</button>{/if}
                      </label>
                    </div>
                    <div class="runrow rh">
                      {#each ['#', 'Payload', 'Status', 'Length', 'Time'] as label, i (label)}
                        <div class="thwrap">
                          <span class="rcell">{label}</span>
                          <!-- svelte-ignore a11y_no_static_element_interactions -->
                          <span
                            class="grip"
                            title="Drag to resize, double-click to reset"
                            onpointerdown={(e) => gripDown(e, 'run', i)}
                            onpointermove={gripMove}
                            onpointerup={gripUp}
                            ondblclick={() => gripReset('run', i)}
                          ></span>
                        </div>
                      {/each}
                    </div>
                    <div class="runbody">
                      {#each runShown as r (r.n)}
                        <button class="runrow" class:sel={r.n === runRow?.n} class:failed={r.error} onclick={() => (runSel = r.n)}>
                          <span class="rcell dim mono">{r.n}</span>
                          <span class="rcell mono pay" title={(r.payloads ?? []).join(', ')}>{(r.payloads ?? []).join(', ') || '-'}</span>
                          <span class="rcell"><span class="rstatus {r.status ? statusClass(r.status) : ''}">{r.error ? 'ERR' : r.status}</span></span>
                          <span class="rcell dim mono">{r.length ?? 0} B</span>
                          <span class="rcell dim mono">{r.timeMs ?? 0} ms</span>
                        </button>
                      {:else}
                        <p class="runempty dim">No result matches "{runQuery}".</p>
                      {/each}
                    </div>
                  </div>
                {/if}
                <div class="msgs" class:with-decoder={miniDecode}>
                  <div class="msgs-panes">
                    <SplitPane storeKey="organizer" left={reqPane} right={resPane} />
                    {#snippet reqPane()}
                      <!-- svelte-ignore a11y_no_static_element_interactions -->
                      <div class="ctxwrap" oncontextmenu={openReqMenu}>
                        <MessageView bind:this={reqRef} title={runRow ? `Request #${runRow.n}` : 'Request'} raw={shownReq} variants={reqVariants} side="req"
                          placeholder={runRow?.trimmed ? 'Request not kept for this row (size cap)' : 'No request saved'} preferPretty
                          ondecode={(t) => (miniDecode = { text: t })} onsend={(t) => store.sendStringToDecoder(t)} />
                      </div>
                    {/snippet}
                    {#snippet resPane()}
                      <!-- svelte-ignore a11y_no_static_element_interactions -->
                      <div class="ctxwrap" oncontextmenu={openResMenu}>
                        <MessageView bind:this={resRef} title={runRow ? `Response #${runRow.n}` : 'Response'} raw={runRow?.error ? '' : shownRes} variants={resVariants} side="res"
                          placeholder={runRow?.error ? runRow.error : runRow?.trimmed ? 'Response not kept for this row (size cap)' : 'No response saved'} preferPretty
                          ondecode={(t) => (miniDecode = { text: t })} onsend={(t) => store.sendStringToDecoder(t)} />
                      </div>
                    {/snippet}
                  </div>
                  {#if miniDecode}
                    <aside class="decoder-dock"><MiniDecoder text={miniDecode.text} onclose={() => (miniDecode = null)} /></aside>
                  {/if}
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
            <div class="ne-mode" role="tablist" aria-label="Note mode">
              <button class="nm" class:active={!notePreviewOn} role="tab" aria-selected={!notePreviewOn} onclick={() => setNoteMode(false)}>Write</button>
              <button class="nm" class:active={notePreviewOn} role="tab" aria-selected={notePreviewOn} onclick={() => setNoteMode(true)}>Preview</button>
            </div>
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
          {#if notePreviewOn}
            <div class="ne-prev">
              {#if curNote.body.trim()}
                <MarkdownView source={curNote.body} />
              {:else}
                <p class="dim ne-prev-empty">Nothing to preview yet.</p>
              {/if}
            </div>
          {:else}
            <textarea
              class="ne-body"
              placeholder="Start writing: findings, payloads, credentials to try, to-dos… Markdown works."
              value={curNote.body}
              spellcheck="false"
              aria-label="Note body"
              oninput={(e) => store.updateNote(curNote.id, { body: e.currentTarget.value })}
            ></textarea>
          {/if}
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

<svelte:window onkeydown={onKey} />

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

{#if capture}
  <CaptureEditor source={capture} onclose={() => (capture = null)} />
{/if}

{#if noteModal}
  <Modal title="Note" draggable onclose={() => (noteModal = null)}>
    <div class="note-modal">
      <div class="nm-head">
        <div class="nm-for mono" title={noteModal.heading}>{noteModal.heading}</div>
        <div class="ne-mode" role="tablist" aria-label="Note mode">
          <button class="nm" class:active={!noteModalPreview} role="tab" aria-selected={!noteModalPreview} onclick={() => (noteModalPreview = false)}>Write</button>
          <button class="nm" class:active={noteModalPreview} role="tab" aria-selected={noteModalPreview} onclick={() => (noteModalPreview = true)}>Preview</button>
        </div>
      </div>
      {#if noteModalPreview}
        <div class="note-area nm-prev">
          {#if noteDraft.trim()}
            <MarkdownView source={noteDraft} />
          {:else}
            <p class="dim ne-prev-empty">Nothing to preview yet.</p>
          {/if}
        </div>
      {:else}
        <textarea
          class="input mono note-area"
          bind:value={noteDraft}
          placeholder="Findings, payloads, reminders… Markdown works."
          spellcheck="false"
          use:autofocus
          onkeydown={(e) => (e.metaKey || e.ctrlKey) && e.key === 'Enter' && saveNote()}
        ></textarea>
      {/if}
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
    /* Set on .rtable from the remembered column widths (drag a header grip). */
    grid-template-columns: var(--cols);
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
  /* Header grip: drag to resize a column, double-click to reset it. */
  .thwrap {
    position: relative;
    display: flex;
    align-items: center;
    min-width: 0;
  }
  .thwrap > .th,
  .thwrap > .rcell {
    flex: 1;
    min-width: 0;
  }
  .grip {
    position: absolute;
    top: 0;
    right: -4px;
    bottom: 0;
    width: 9px;
    cursor: col-resize;
    touch-action: none;
    z-index: 2;
  }
  .grip::after {
    content: '';
    position: absolute;
    top: 5px;
    bottom: 5px;
    left: 4px;
    width: 1px;
    background: var(--border);
  }
  .grip:hover::after {
    background: var(--accent);
  }
  /* A saved Intruder run: its results as a compact table above the message panes. */
  .runbar {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--border);
    font-size: 12px;
  }
  .runsearch {
    width: 230px;
  }
  .runempty {
    padding: 10px 12px;
    font-size: 12px;
  }
  .runrows {
    display: flex;
    flex-direction: column;
    min-height: 0;
    max-height: 230px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }
  .runbody {
    min-height: 0;
    overflow: auto;
  }
  .runrow {
    display: grid;
    grid-template-columns: var(--cols);
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 0 12px;
    height: 26px;
    border: 0;
    border-bottom: 1px solid color-mix(in srgb, var(--border) 45%, transparent);
    background: none;
    color: var(--text);
    font-size: 12px;
    text-align: left;
    cursor: default;
  }
  .runrow.rh {
    position: sticky;
    top: 0;
    color: var(--text-faint);
    font: 600 10.5px var(--sans);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    background: var(--bg-elev);
  }
  .runbody .runrow:hover {
    background: var(--bg-hover);
  }
  .runbody .runrow.sel {
    background: var(--accent-soft);
  }
  .runrow .rcell {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .runrow .pay {
    color: var(--text);
  }
  .runrow.failed .rstatus {
    color: var(--red);
  }
  .rstatus {
    font: 600 11.5px var(--mono);
  }
  /* Grid (not flex) so the panes fill the full width, and a second column opens for
     the decoder dock when a selection is being decoded (same layout as History). */
  .msgs {
    display: grid;
    grid-template-columns: 1fr;
    grid-template-rows: minmax(0, 1fr);
    flex: 1;
    min-height: 0;
  }
  .msgs.with-decoder {
    /* The dock column sizes to the MiniDecoder's own (draggable) width. */
    grid-template-columns: minmax(0, 1fr) auto;
  }
  .msgs-panes {
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    min-width: 0;
    min-height: 0;
  }
  .decoder-dock {
    min-width: 0;
    min-height: 0;
  }
  /* Transparent to layout (MessageView stays the SplitPane grid child) but still on
     the DOM path, so right-clicking anywhere in the pane reaches oncontextmenu. */
  .ctxwrap {
    display: contents;
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
  /* Write / Preview switch, shared by the Notes tab and the per-request note modal. */
  .ne-mode {
    display: inline-flex;
    padding: 2px;
    border-radius: 6px;
    background: var(--bg);
    border: 1px solid var(--border);
  }
  .ne-mode .nm {
    padding: 2px 9px;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--text-dim);
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .ne-mode .nm:hover {
    color: var(--text);
  }
  .ne-mode .nm.active {
    background: var(--bg-elev);
    color: var(--text);
    font-weight: 600;
  }
  /* Preview shares the editor's left gutter so toggling does not shift the text. */
  .ne-prev {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 6px 12px 22px 12px;
  }
  .ne-prev-empty {
    margin: 0;
    font-size: 12.5px;
  }
  .nm-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .nm-head .nm-for {
    min-width: 0;
  }
  .nm-head .ne-mode {
    margin-left: auto;
    flex: none;
  }
  .nm-prev {
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
  }
  .nm-prev .ne-prev-empty {
    padding: 8px 12px;
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
