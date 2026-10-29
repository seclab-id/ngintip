<script>
  // Settings > Vault: a two-pane credential manager. Each credential is a label, a role,
  // a note and any mix of typed fields (username, email, password, API key, token, URL,
  // custom), so it can be just an API key, an email + password, a token, and so on. The
  // list never shows secret values; the editor saves as you type.
  import { tick } from 'svelte';
  import { store } from '../lib/store.svelte.js';
  import ContextMenu from './ContextMenu.svelte';
  import { fmtDateTime } from '../lib/format.js';
  import { FIELD_KINDS, VAULT_TEMPLATES, avatarColor, basicFor, bearerFor, kindInfo, normalizeUrl, vaultName, vaultSubtitle, vaultView } from '../lib/vault.js';

  let query = $state('');
  let selId = $state(null);
  let revealed = $state(new Set()); // field ids shown in clear
  let copied = $state(''); // what was just copied (check-mark feedback)
  let menu = $state(null);
  let titleEl = $state();

  const entries = $derived(store.vault.entries);
  const shown = $derived(vaultView(entries, query));
  const cur = $derived(entries.find((e) => e.id === selId) ?? null);
  const basic = $derived(cur ? basicFor(cur) : '');
  const bearer = $derived(cur ? bearerFor(cur) : '');

  // Keep something selected: the first entry, or the next one after a delete.
  $effect(() => {
    if (!cur && shown.length) selId = shown[0].id;
  });
  // Revealed secrets never carry over to another entry.
  $effect(() => {
    void selId;
    revealed = new Set();
  });

  const UNTITLED = 'Untitled credential';
  const initial = (e) => (vaultName(e) === UNTITLED ? '?' : vaultName(e)[0].toUpperCase());
  const preview = (v) => (String(v).length > 26 ? `${String(v).slice(0, 25)}…` : String(v));
  const fieldName = (f) => f.name.trim() || kindInfo(f.kind).name;

  async function addEntry(template) {
    query = '';
    selId = store.addVaultEntry(template);
    await tick();
    titleEl?.focus();
  }
  function openTemplateMenu(ev) {
    const r = ev.currentTarget.getBoundingClientRect();
    menu = { x: r.left, y: r.bottom + 4, items: VAULT_TEMPLATES.map((t) => ({ label: t.label, hint: t.hint, run: () => addEntry(t.id) })) };
  }

  const setEntry = (key, value) => cur && store.updateVaultEntry(cur.id, { [key]: value });
  const setField = (f, patch) => cur && store.updateVaultField(cur.id, f.id, patch);
  async function addField(kind) {
    if (!cur) return;
    const fid = store.addVaultField(cur.id, kind);
    await tick();
    document.querySelector(`.vault [data-field="${fid}"] .fval`)?.focus();
  }
  function toggleReveal(id) {
    const next = new Set(revealed);
    next.has(id) ? next.delete(id) : next.add(id);
    revealed = next;
  }

  let copiedTimer;
  async function copy(key, text, what) {
    if (!String(text ?? '')) return;
    await store.copyText(text, what);
    copied = key;
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied = ''), 1200);
  }
  const openUrl = (url) => store.openPentestBrowser(normalizeUrl(url));

  async function removeEntry(e) {
    const ok = await store.confirm({ title: `Delete credential "${vaultName(e)}"?`, message: 'All of its fields are removed from the vault. This cannot be undone.' });
    if (ok) store.removeVaultEntry(e.id);
  }
  // An empty field goes at once; one holding a value asks first.
  async function removeField(id, f) {
    if (f.value && !(await store.confirm({ title: `Remove field "${fieldName(f)}"?`, message: 'Its value is deleted from this credential. This cannot be undone.', yes: 'Yes, remove' }))) return;
    store.removeVaultField(id, f.id);
  }

  /** Field options: copy, type, secrecy, order, remove (right-click or the ⋯ button). */
  function openFieldMenu(f, i, x, y) {
    const id = cur.id;
    menu = {
      x,
      y,
      items: [
        { label: 'Copy value', disabled: !f.value, run: () => copy(f.id, f.value, fieldName(f)) },
        { label: 'Type', submenu: FIELD_KINDS.map((k) => ({ label: k.name, hint: k.kind === f.kind ? 'current' : k.secret ? 'secret' : '', run: () => store.setVaultFieldKind(id, f.id, k.kind) })) },
        { label: f.secret ? 'Show as plain text' : 'Mark as secret', run: () => store.setVaultFieldSecret(id, f.id, !f.secret) },
        'separator',
        { label: 'Move up', disabled: i === 0, run: () => store.moveVaultField(id, f.id, -1) },
        { label: 'Move down', disabled: i === cur.fields.length - 1, run: () => store.moveVaultField(id, f.id, 1) },
        'separator',
        { label: f.value ? 'Remove field…' : 'Remove field', danger: true, run: () => removeField(id, f) },
      ],
    };
  }

  /** Credential options in the list: copy any field, ready auth headers, open, duplicate. */
  function openEntryMenu(e, x, y) {
    selId = e.id;
    const withValue = e.fields.filter((f) => String(f.value).trim());
    const url = e.fields.find((f) => f.kind === 'url' && f.value.trim())?.value;
    const b = basicFor(e);
    const t = bearerFor(e);
    menu = {
      x,
      y,
      items: [
        { label: 'Copy', submenu: withValue.map((f) => ({ label: fieldName(f), hint: f.secret ? '••••••' : preview(f.value), run: () => copy('', f.value, fieldName(f)) })) },
        ...(b ? [{ label: 'Copy Basic auth header', run: () => copy('', b, 'Authorization header') }] : []),
        ...(t ? [{ label: 'Copy Bearer header', run: () => copy('', t, 'Authorization header') }] : []),
        'separator',
        { label: 'Open URL in proxied browser', disabled: !url, run: () => openUrl(url) },
        { label: 'Duplicate', run: () => (selId = store.duplicateVaultEntry(e.id)) },
        'separator',
        { label: 'Delete…', danger: true, run: () => removeEntry(e) },
      ],
    };
  }
</script>

{#snippet copyBtn(key, text, what)}
  <button class="icbtn" title="Copy {what}" aria-label="Copy {what}" disabled={!text} onclick={() => copy(key, text, what)}>
    {#if copied === key}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" class="ok"><path d="M20 6 9 17l-5-5" /></svg>
    {:else}
      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a2 2 0 0 1 2-2h10" /></svg>
    {/if}
  </button>
{/snippet}

{#snippet keyIcon(size)}
  <svg viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="7.5" cy="15.5" r="4.5" /><path d="m10.7 12.3 9.8-9.8M17 6l3 3M14.5 8.5l2.5 2.5" /></svg>
{/snippet}

<div class="vault">
  <header class="vhead">
    <div class="vtitle">
      <h2>Vault</h2>
      <p class="dim">Target credentials kept with the project: test accounts per role, emails, passwords, API keys, tokens.</p>
    </div>
    <span class="grow"></span>
    {#if entries.length}
      <label class="search">
        <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></svg>
        <input placeholder="Search label, role, user, host…" bind:value={query} spellcheck="false" aria-label="Search credentials" />
        {#if query}<button class="clear" aria-label="Clear search" onclick={() => (query = '')}>×</button>{/if}
      </label>
      <button class="btn primary sm" onclick={openTemplateMenu}>+ New credential ▾</button>
    {/if}
  </header>

  {#if entries.length}
    <div class="vbody">
      <aside class="vlist" aria-label="Credentials">
        <div class="vlist-scroll">
          {#each shown as e (e.id)}
            <button class="vitem" class:sel={e.id === selId} onclick={() => (selId = e.id)} oncontextmenu={(ev) => (ev.preventDefault(), openEntryMenu(e, ev.clientX, ev.clientY))}>
              <span class="avatar c-{avatarColor(e.id)}">{initial(e)}</span>
              <span class="vmeta">
                <span class="vname-row">
                  <span class="vname" class:untitled={vaultName(e) === UNTITLED}>{vaultName(e)}</span>
                  {#if e.role?.trim()}<span class="role">{e.role.trim()}</span>{/if}
                </span>
                <span class="vsub">{vaultSubtitle(e)}</span>
              </span>
            </button>
          {:else}
            <p class="vempty">No credential matches "{query}".</p>
          {/each}
        </div>
        <div class="vlist-foot">{entries.length} credential{entries.length === 1 ? '' : 's'} · right-click for quick copy</div>
      </aside>

      <section class="vdetail">
        {#if cur}
          <div class="dtop">
            <span class="avatar lg c-{avatarColor(cur.id)}">{initial(cur)}</span>
            <div class="dtitle">
              <input bind:this={titleEl} class="ghost title" value={cur.label} placeholder={UNTITLED} spellcheck="false" aria-label="Label" oninput={(ev) => setEntry('label', ev.currentTarget.value)} />
              <input class="ghost role-in" value={cur.role} placeholder="+ add a role (admin, user, guest…)" spellcheck="false" aria-label="Role" oninput={(ev) => setEntry('role', ev.currentTarget.value)} />
            </div>
            <div class="dactions">
              {#if basic}
                <button class="btn ghost sm" title="Copy an Authorization: Basic header from the username / email and password" onclick={() => copy('basic', basic, 'Authorization header')}>
                  {copied === 'basic' ? 'Copied ✓' : 'Copy Basic auth'}
                </button>
              {/if}
              {#if bearer}
                <button class="btn ghost sm" title="Copy an Authorization: Bearer header with the token" onclick={() => copy('bearer', bearer, 'Authorization header')}>
                  {copied === 'bearer' ? 'Copied ✓' : 'Copy Bearer'}
                </button>
              {/if}
              <button class="icbtn" title="Duplicate" aria-label="Duplicate credential" onclick={() => (selId = store.duplicateVaultEntry(cur.id))}>
                <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
              </button>
              <button class="icbtn danger" title="Delete" aria-label="Delete credential" onclick={() => removeEntry(cur)}>
                <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" /></svg>
              </button>
            </div>
          </div>

          <div class="fields">
            {#each cur.fields as f, i (f.id)}
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="field" class:secret={f.secret} data-field={f.id} oncontextmenu={(ev) => (ev.preventDefault(), openFieldMenu(f, i, ev.clientX, ev.clientY))}>
                <input class="fname" value={f.name} placeholder={kindInfo(f.kind).name} spellcheck="false" aria-label="Field name" oninput={(ev) => setField(f, { name: ev.currentTarget.value })} />
                <input
                  class="finput fval"
                  type={f.secret && !revealed.has(f.id) ? 'password' : 'text'}
                  value={f.value}
                  placeholder={kindInfo(f.kind).placeholder}
                  autocomplete={f.secret ? 'new-password' : 'off'}
                  spellcheck="false"
                  aria-label={fieldName(f)}
                  oninput={(ev) => setField(f, { value: ev.currentTarget.value })}
                />
                <span class="factions">
                  {#if f.secret}
                    <button class="icbtn" title={revealed.has(f.id) ? `Hide ${fieldName(f)}` : `Show ${fieldName(f)}`} aria-label={revealed.has(f.id) ? `Hide ${fieldName(f)}` : `Show ${fieldName(f)}`} onclick={() => toggleReveal(f.id)}>
                      {#if revealed.has(f.id)}
                        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17.9 17.9A10 10 0 0 1 12 20c-7 0-11-8-11-8a18.5 18.5 0 0 1 5.1-5.9M9.9 4.2A9 9 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.2 3.2M14.1 14.1a3 3 0 1 1-4.2-4.2M1 1l22 22" /></svg>
                      {:else}
                        <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8S1 12 1 12z" /><circle cx="12" cy="12" r="3" /></svg>
                      {/if}
                    </button>
                  {/if}
                  {#if f.kind === 'url'}
                    <button class="icbtn" title="Open in the proxied browser" aria-label="Open URL" disabled={!f.value.trim()} onclick={() => openUrl(f.value)}>
                      <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6M15 3h6v6M10 14 21 3" /></svg>
                    </button>
                  {/if}
                  {@render copyBtn(f.id, f.value, fieldName(f))}
                  <button class="icbtn" title="Field options" aria-label="Field options" onclick={(ev) => { const r = ev.currentTarget.getBoundingClientRect(); openFieldMenu(f, i, r.left, r.bottom + 4); }}>
                    <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor"><circle cx="5" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="19" cy="12" r="1.8" /></svg>
                  </button>
                </span>
              </div>
            {:else}
              <p class="nofields">No fields yet. Add whatever this credential needs below.</p>
            {/each}
            <div class="addrow">
              <span class="addlabel">Add</span>
              {#each FIELD_KINDS as k (k.kind)}
                <button class="chip" title="Add a {k.name.toLowerCase()} field{k.secret ? ' (secret)' : ''}" onclick={() => addField(k.kind)}>+ {k.name}</button>
              {/each}
            </div>
          </div>

          <label class="note-field">
            <span class="flabel">Note</span>
            <textarea class="finput note" value={cur.note} placeholder="Where it came from, 2FA / OTP seed, scope, lockout rules…" spellcheck="false" oninput={(ev) => setEntry('note', ev.currentTarget.value)}></textarea>
          </label>

          <div class="dfoot">Created {fmtDateTime(cur.createdAt)} · Updated {fmtDateTime(cur.updatedAt)} · saves as you type · right-click a field for more</div>
        {/if}
      </section>
    </div>
  {:else}
    <div class="vblank">
      <div class="vblank-icon">{@render keyIcon(24)}</div>
      <h3>No credentials yet</h3>
      <p>Keep the target's test accounts (one per role, for access-control testing), API keys and tokens here. Pick what kind of credential to start with; you can add or change fields any time.</p>
      <div class="templates">
        {#each VAULT_TEMPLATES as t (t.id)}
          <button class="tcard" onclick={() => addEntry(t.id)}>
            <span class="tname">{t.label}</span>
            <span class="thint">{t.hint}</span>
          </button>
        {/each}
      </div>
    </div>
  {/if}
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}


<style>
  .vault {
    display: flex;
    flex-direction: column;
    gap: 12px;
    max-width: 1100px;
    height: 100%;
    min-height: 480px;
  }
  .grow {
    flex: 1;
  }
  .vhead {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .vtitle h2 {
    margin: 0 0 2px;
    font-size: 16px;
    font-weight: 650;
  }
  .vtitle p {
    margin: 0;
    font-size: 12.5px;
  }
  .search {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 250px;
    height: 30px;
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

  .vbody {
    display: grid;
    grid-template-columns: 300px 1fr;
    flex: 1;
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
    overflow: hidden;
  }

  /* List */
  .vlist {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-right: 1px solid var(--border);
  }
  .vlist-scroll {
    flex: 1;
    overflow-y: auto;
    padding: 6px;
  }
  .vitem {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 8px 9px;
    border: 0;
    border-radius: 8px;
    background: none;
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .vitem:hover {
    background: var(--bg-hover);
  }
  .vitem.sel {
    background: var(--accent-soft);
  }
  .avatar {
    display: grid;
    flex: none;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 9px;
    background: color-mix(in srgb, var(--hl) 20%, transparent);
    color: var(--hl);
    font: 700 13px var(--sans);
  }
  .avatar.lg {
    width: 46px;
    height: 46px;
    border-radius: 12px;
    font-size: 19px;
  }
  .vmeta {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .vname-row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .vname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font: 600 13px var(--sans);
  }
  .vname.untitled {
    color: var(--text-faint);
    font-style: italic;
    font-weight: 500;
  }
  .role {
    flex: none;
    max-width: 110px;
    overflow: hidden;
    padding: 0 7px;
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 600 10.5px/17px var(--sans);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .vsub {
    overflow: hidden;
    color: var(--text-faint);
    font-size: 11.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .vempty {
    margin: 12px 8px;
    color: var(--text-faint);
    font-size: 12px;
  }
  .vlist-foot {
    padding: 8px 12px;
    border-top: 1px solid var(--border);
    color: var(--text-faint);
    font-size: 11px;
  }

  /* Detail */
  .vdetail {
    display: flex;
    flex-direction: column;
    gap: 16px;
    min-width: 0;
    min-height: 0;
    padding: 18px 22px;
    overflow-y: auto;
  }
  .dtop {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .dtitle {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .ghost {
    width: 100%;
    margin-left: -7px;
    padding: 3px 6px;
    border: 1px solid transparent;
    border-radius: 6px;
    outline: 0;
    background: none;
    color: var(--text);
  }
  .ghost:hover {
    border-color: var(--border);
  }
  .ghost:focus {
    border-color: var(--accent);
    background: var(--bg);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .ghost.title {
    font: 700 19px/1.3 var(--sans);
  }
  .ghost.role-in {
    color: var(--text-dim);
    font: 500 12px var(--sans);
  }
  .ghost::placeholder,
  .finput::placeholder,
  .fname::placeholder {
    color: var(--text-faint);
  }
  .dactions {
    display: flex;
    flex: none;
    align-items: center;
    gap: 4px;
  }

  .fields {
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg);
    overflow: hidden;
  }
  .field {
    display: grid;
    grid-template-columns: 140px 1fr auto;
    align-items: center;
    gap: 8px;
    padding: 5px 6px 5px 8px;
    border-bottom: 1px solid var(--border);
  }
  .field.secret {
    box-shadow: inset 2px 0 0 color-mix(in srgb, var(--amber) 70%, transparent);
  }
  .fname {
    width: 100%;
    min-width: 0;
    padding: 5px 6px;
    border: 1px solid transparent;
    border-radius: 6px;
    outline: 0;
    background: none;
    color: var(--text-dim);
    font: 600 11.5px var(--sans);
  }
  .fname:hover {
    border-color: var(--border);
  }
  .fname:focus {
    border-color: var(--accent);
    background: var(--bg-elev);
    color: var(--text);
  }
  .finput {
    width: 100%;
    min-width: 0;
    padding: 6px 8px;
    border: 1px solid transparent;
    border-radius: 6px;
    outline: 0;
    background: none;
    color: var(--text);
    font: 13px var(--mono);
  }
  .finput:hover {
    border-color: var(--border);
  }
  .finput:focus {
    border-color: var(--accent);
    background: var(--bg-elev);
    box-shadow: 0 0 0 3px var(--accent-soft);
  }
  .factions {
    display: flex;
    gap: 1px;
  }
  .nofields {
    margin: 0;
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
    color: var(--text-faint);
    font-size: 12px;
  }
  .addrow {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 5px;
    padding: 8px 10px;
  }
  .addlabel {
    margin-right: 2px;
    color: var(--text-faint);
    font: 700 10.5px var(--sans);
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .chip {
    padding: 3px 9px;
    border: 1px dashed var(--border);
    border-radius: 99px;
    background: none;
    color: var(--text-dim);
    font: 500 11.5px var(--sans);
    cursor: pointer;
  }
  .chip:hover {
    border-style: solid;
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--accent);
  }
  .note-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .flabel {
    color: var(--text-faint);
    font: 700 10.5px var(--sans);
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }
  .finput.note {
    min-height: 110px;
    padding: 10px 12px;
    border-color: var(--border);
    background: var(--bg);
    font: 13px/1.55 var(--sans);
    resize: vertical;
  }
  .dfoot {
    color: var(--text-faint);
    font-size: 11px;
  }

  .icbtn {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border: 0;
    border-radius: 6px;
    background: none;
    color: var(--text-dim);
    cursor: pointer;
  }
  .icbtn:hover:not(:disabled) {
    background: var(--bg-hover);
    color: var(--text);
  }
  .icbtn:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .icbtn.danger:hover:not(:disabled) {
    color: var(--red);
  }
  .ok {
    color: var(--green);
  }

  /* Empty state */
  .vblank {
    display: flex;
    flex: 1;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 32px 24px;
    border: 1px dashed var(--border);
    border-radius: var(--radius-lg);
    text-align: center;
  }
  .vblank h3 {
    margin: 4px 0 0;
    font-size: 15px;
  }
  .vblank p {
    max-width: 460px;
    margin: 0 0 10px;
    color: var(--text-dim);
    font-size: 12.5px;
    line-height: 1.55;
  }
  .vblank-icon {
    display: grid;
    place-items: center;
    width: 50px;
    height: 50px;
    border-radius: 14px;
    background: var(--accent-soft);
    color: var(--accent);
  }
  .templates {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(130px, 1fr));
    gap: 8px;
    width: min(720px, 100%);
  }
  .tcard {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--bg-elev);
    color: var(--text);
    text-align: left;
    cursor: pointer;
  }
  .tcard:hover {
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .tname {
    font: 600 13px var(--sans);
  }
  .thint {
    color: var(--text-faint);
    font-size: 11.5px;
  }
</style>
