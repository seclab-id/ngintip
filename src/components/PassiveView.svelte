<script>
  import { store } from '../lib/store.svelte.js';
  import { SEVERITY_ORDER, SEVERITIES, ruleError, parseScope, DEFAULT_PASSIVE_SCOPE } from '../lib/passive.js';
  import Modal from './Modal.svelte';

  let sev = $state('all'); // severity filter

  let findings = $derived(sev === 'all' ? store.passiveFindings : store.passiveFindings.filter((f) => f.severity === sev));
  let counts = $derived.by(() => {
    const c = { high: 0, medium: 0, low: 0, info: 0 };
    for (const f of store.passiveFindings) c[f.severity] = (c[f.severity] ?? 0) + 1;
    return c;
  });
  let p = $derived(store.passive);
  const sevName = (rank) => SEVERITIES[rank] ?? 'info';

  // Group findings by their source resource, so e.g. main.js shows once with all
  // its matched rules (deduped) instead of one row per match.
  let grouped = $derived.by(() => {
    const map = new Map();
    for (const f of findings) {
      const key = `${f.host}${f.path}`;
      let g = map.get(key);
      if (!g) {
        g = { key, host: f.host, path: f.path, method: f.method, flowId: f.flowId, items: [], seen: new Set(), rank: 9 };
        map.set(key, g);
      }
      const dk = `${f.type}|${f.where}|${f.match}`;
      if (!g.seen.has(dk)) {
        g.seen.add(dk);
        g.items.push(f);
      }
      g.rank = Math.min(g.rank, SEVERITY_ORDER[f.severity]);
      g.flowId = f.flowId; // jump to the most recent flow for this resource
    }
    const arr = [...map.values()];
    for (const g of arr) g.items.sort((a, b) => SEVERITY_ORDER[a.severity] - SEVERITY_ORDER[b.severity]);
    arr.sort((a, b) => a.rank - b.rank || a.host.localeCompare(b.host));
    return arr;
  });

  // Expand/collapse of a finding group is persisted per project (survives view
  // switches and restarts). Default is expanded (key not in the collapsed set).
  const isCollapsed = (key) => store.passiveGroupCollapsed(key);
  let allCollapsed = $derived(grouped.length > 0 && grouped.every((g) => store.passiveGroupCollapsed(g.key)));
  function toggleAll() {
    store.setPassiveCollapsed(allCollapsed ? [] : grouped.map((g) => g.key));
  }

  // Custom rule editor.
  let rulesOpen = $state(false);
  let draft = $state({ title: '', severity: 'medium', pattern: '', flags: '' });
  let ruleErr = $derived(draft.pattern.trim() ? ruleError(draft) : '');

  function addRule() {
    if (!draft.pattern.trim() || ruleErr) return;
    if (!store.addPassiveRule(draft)) draft = { title: '', severity: 'medium', pattern: '', flags: '' };
  }

  // Scan scope: which resources (by extension / content-type) get scanned.
  let scopeAdd = $state('');
  let scopeAll = $derived(store.passiveScope.includes('*'));
  function addScopeExt() {
    const parts = parseScope(scopeAdd);
    if (!parts.length) return;
    if (parts.includes('*')) return store.setPassiveScope(['*']), void (scopeAdd = '');
    const next = scopeAll ? [] : [...store.passiveScope];
    for (const p of parts) if (!next.includes(p)) next.push(p);
    store.setPassiveScope(next);
    scopeAdd = '';
  }
  const removeScopeExt = (ext) => store.setPassiveScope(store.passiveScope.filter((e) => e !== ext));

  // Selection: pick findings to copy or delete. Keys are the finding's unique key.
  let selecting = $state(false);
  let selected = $state(new Set());
  let visibleKeys = $derived(findings.map((f) => f.key));

  function toggleSelecting() {
    selecting = !selecting;
    if (!selecting) selected = new Set();
  }
  function toggleItem(key) {
    const next = new Set(selected);
    next.has(key) ? next.delete(key) : next.add(key);
    selected = next;
  }
  function groupState(g) {
    const on = g.items.reduce((n, it) => n + (selected.has(it.key) ? 1 : 0), 0);
    return on === 0 ? 'none' : on === g.items.length ? 'all' : 'some';
  }
  function toggleGroup(g) {
    const next = new Set(selected);
    const all = g.items.every((it) => next.has(it.key));
    for (const it of g.items) all ? next.delete(it.key) : next.add(it.key);
    selected = next;
  }
  function copySelected() {
    store.copyPassiveFindings(store.passiveFindings.filter((f) => selected.has(f.key)));
  }
  function deleteSelected() {
    store.deletePassiveFindings(selected);
    selected = new Set();
  }

  // Detail modal: inspect a finding's data in place (which resource, the match,
  // where) instead of jumping to History. `focus` highlights one finding.
  let detail = $state(null); // { group, focus } | null
  const openDetail = (group, focus = null) => (detail = { group, focus });
  const closeDetail = () => (detail = null);
  function rowClick(g, f) {
    if (selecting) toggleItem(f.key);
    else openDetail(g, f);
  }
</script>

<div class="passive">
  <header class="bar">
    <button class="btn" onclick={() => store.scanPassive()} disabled={p.scanning || !store.flows.length}>
      {p.scanning ? `Scanning ${p.done}/${p.total}…` : 'Scan history'}
    </button>
    <button class="btn ghost sm" onclick={() => store.scanPassive({ rescan: true })} disabled={p.scanning || !store.flows.length}>Rescan all</button>
    <label class="inscope" title="Only scan flows whose host is in the project Scope (Settings > Scope). Empty scope = everything.">
      <input type="checkbox" checked={store.passiveInScopeOnly} onchange={(e) => store.setPassiveInScopeOnly(e.currentTarget.checked)} />
      In scope only
    </label>
    <button class="btn ghost sm" onclick={() => store.clearPassive()} disabled={p.scanning || !store.passiveFindings.length}>Clear</button>
    <button class="btn ghost sm" class:on={rulesOpen} onclick={() => (rulesOpen = !rulesOpen)}>
      Rules{store.passiveRules.length ? ` (+${store.passiveRules.length})` : ''}
    </button>
    <button class="btn ghost sm" class:on={selecting} onclick={toggleSelecting} disabled={!store.passiveFindings.length}>
      {selecting ? 'Done' : 'Select'}
    </button>
    {#if selecting}
      <button class="btn ghost sm" onclick={() => (selected = new Set(visibleKeys))} disabled={!findings.length}>All</button>
      <button class="btn ghost sm" onclick={() => (selected = new Set())} disabled={!selected.size}>None</button>
      <button class="btn ghost sm" onclick={copySelected} disabled={!selected.size}>Copy ({selected.size})</button>
      <button class="btn ghost sm danger" onclick={deleteSelected} disabled={!selected.size}>Delete ({selected.size})</button>
    {/if}
    <button class="btn ghost sm" onclick={toggleAll} disabled={!grouped.length} title="Expand or collapse all finding groups">
      {allCollapsed ? 'Expand all' : 'Collapse all'}
    </button>

    <div class="spacer"></div>
    <div class="sevs" role="tablist">
      <button class="sevchip" class:on={sev === 'all'} onclick={() => (sev = 'all')}>All {store.passiveFindings.length}</button>
      {#each SEVERITIES as s}
        {#if counts[s]}
          <button class="sevchip sev-{s}" class:on={sev === s} onclick={() => (sev = s)}>{s} {counts[s]}</button>
        {/if}
      {/each}
    </div>
  </header>

  {#if rulesOpen}
    <div class="rules">
      <div class="rules-head">
        <span class="cap">Scan scope</span>
        <span class="dim">Which resources get scanned, by extension / content-type. Use <span class="mono">*</span> for everything.</span>
      </div>
      <div class="scope">
        <div class="chips">
          {#if scopeAll}
            <span class="chip all">all (*)</span>
          {:else if store.passiveScope.length}
            {#each store.passiveScope as ext (ext)}
              <span class="chip">{ext}<button class="chipx" title="Remove" aria-label="Remove {ext}" onclick={() => removeScopeExt(ext)}>×</button></span>
            {/each}
          {:else}
            <span class="dim">nothing selected — nothing will be scanned</span>
          {/if}
        </div>
        <div class="scope-add">
          <input class="input mono ext" placeholder="add ext, e.g. js, json, *" bind:value={scopeAdd} spellcheck="false"
            onkeydown={(e) => e.key === 'Enter' && addScopeExt()} />
          <button class="btn sm" onclick={addScopeExt} disabled={!scopeAdd.trim()}>Add</button>
          <button class="btn ghost sm" class:on={scopeAll} onclick={() => store.setPassiveScope(['*'])}>All (*)</button>
          <button class="btn ghost sm" onclick={() => store.setPassiveScope([...DEFAULT_PASSIVE_SCOPE])}>Reset</button>
        </div>
      </div>

      <div class="rules-head">
        <span class="cap">Scan rules</span>
        <span class="dim">Toggle any rule; source is <span class="mono">src/lib/passive-rules.js</span>. Rules and toggles are saved per project.</span>
      </div>

      <div class="rule-list">
        {#each store.builtinRules as r (r.id)}
          <label class="rule" class:off={!store.ruleEnabled(r.id)}>
            <input type="checkbox" checked={store.ruleEnabled(r.id)} onchange={() => store.toggleBuiltinRule(r.id)} />
            <span class="sev sev-{r.severity}">{r.severity}</span>
            <span class="rtitle">{r.title} <span class="tag">built-in</span></span>
            <span class="rpattern mono">/{r.pattern}/{r.flags}</span>
            <span class="del-spacer"></span>
          </label>
        {/each}
        {#each store.passiveRules as r (r.id)}
          <label class="rule" class:off={r.enabled === false}>
            <input type="checkbox" checked={r.enabled !== false} onchange={() => store.toggleCustomRule(r.id)} />
            <span class="sev sev-{r.severity}">{r.severity}</span>
            <span class="rtitle">{r.title} <span class="tag custom">custom</span></span>
            <span class="rpattern mono">/{r.pattern}/{r.flags}</span>
            <button class="del" title="Delete rule" aria-label="Delete rule" onclick={(e) => { e.preventDefault(); store.removePassiveRule(r.id); }}>×</button>
          </label>
        {/each}
      </div>

      <div class="rule-add">
        <input class="input title" placeholder="Title" bind:value={draft.title} spellcheck="false" />
        <select class="input sev-sel" bind:value={draft.severity}>
          {#each SEVERITIES as s}<option value={s}>{s}</option>{/each}
        </select>
        <input class="input mono pat" placeholder="regex pattern, e.g. \bsecret_[0-9a-f]{'{'}8{'}'}\b" bind:value={draft.pattern} spellcheck="false"
          onkeydown={(e) => e.key === 'Enter' && addRule()} />
        <input class="input mono flags" placeholder="flags (i)" bind:value={draft.flags} spellcheck="false" />
        <button class="btn sm" onclick={addRule} disabled={!draft.pattern.trim() || !!ruleErr}>Add rule</button>
      </div>
      {#if ruleErr}<p class="rule-err">{ruleErr}</p>{/if}
    </div>
  {/if}

  {#if p.scanning}
    <div class="progress"><div class="fill" style="width: {p.total ? (p.done / p.total) * 100 : 0}%"></div></div>
  {/if}

  {#if !store.passiveFindings.length}
    <div class="empty">
      <p>No findings yet.</p>
      <p class="dim">Click <b>Scan history</b> to passively analyze captured traffic for secrets, tokens, and risky response headers.</p>
    </div>
  {:else}
    <div class="list">
      {#each grouped as g (g.key)}
        <div class="group">
          <div class="grow">
            {#if selecting}
              <input
                type="checkbox"
                class="gcheck"
                aria-label="Select all findings for this resource"
                checked={groupState(g) === 'all'}
                indeterminate={groupState(g) === 'some'}
                onchange={() => toggleGroup(g)}
              />
            {/if}
            <button
              class="caret"
              aria-label="Expand or collapse findings"
              aria-expanded={!isCollapsed(g.key)}
              title={isCollapsed(g.key) ? 'Expand findings' : 'Collapse findings'}
              onclick={() => store.togglePassiveGroup(g.key)}
            >{isCollapsed(g.key) ? '▸' : '▾'}</button>
            <button class="ghead" onclick={() => openDetail(g)} title="Show finding details">
              <span class="sev sev-{sevName(g.rank)}">{sevName(g.rank)}</span>
              <span class="gsrc"><span class="m m-{g.method?.toLowerCase()}">{g.method}</span> {g.host}{g.path}</span>
              <span class="gcount">{g.items.length} finding{g.items.length === 1 ? '' : 's'}</span>
            </button>
          </div>
          {#if !isCollapsed(g.key)}
          <div class="gitems" class:selmode={selecting}>
            {#each g.items as f (f.key)}
              <div class="gitem" class:sel={selected.has(f.key)} role="button" tabindex="0" title="Show details"
                onclick={() => rowClick(g, f)}
                onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && (e.preventDefault(), rowClick(g, f))}>
                {#if selecting}
                  <input type="checkbox" class="icheck" aria-label="Select finding" checked={selected.has(f.key)} onclick={(e) => e.stopPropagation()} onchange={() => toggleItem(f.key)} />
                {/if}
                <span class="sev sev-{f.severity}">{f.severity}</span>
                <span class="ftitle">{f.title}</span>
                <span class="match mono">{f.match}</span>
                <button class="copy1" title="Copy match" aria-label="Copy match" onclick={(e) => { e.stopPropagation(); store.copyText(f.match, 'Match'); }}>⧉</button>
                <span class="where">{f.where === 'request' ? 'req' : 'res'}</span>
              </div>
            {/each}
          </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  {#if detail}
    {@const g = detail.group}
    <Modal title="Finding detail" onclose={closeDetail}>
      <div class="dmeta">
        <span class="sev sev-{sevName(g.rank)}">{sevName(g.rank)}</span>
        <span class="dsrc mono"><span class="m m-{g.method?.toLowerCase()}">{g.method}</span> {g.host}{g.path}</span>
      </div>
      <div class="dgrid">
        <span class="dk">Flow</span><span class="dv mono">#{g.flowId}</span>
        <span class="dk">Findings</span><span class="dv">{g.items.length}</span>
      </div>
      <div class="dfindings">
        {#each g.items as f (f.key)}
          <div class="dfinding" class:focus={detail.focus && detail.focus.key === f.key}>
            <div class="dfhead">
              <span class="sev sev-{f.severity}">{f.severity}</span>
              <span class="dftitle">{f.title}</span>
              <span class="where">{f.where === 'request' ? 'req' : 'res'}</span>
              <span class="dflow mono" title="Source request #{f.flowId}">#{f.flowId}</span>
              <button class="btn ghost sm" onclick={() => store.copyText(f.match, 'Match')}>Copy</button>
              <button
                class="btn sm"
                title="Open this exact request in History and highlight the match"
                onclick={() => { store.showFinding(f.flowId, f); closeDetail(); }}
              >
                Lookup to Source
              </button>
            </div>
            <pre class="dmatch mono">{f.match}</pre>
          </div>
        {/each}
      </div>
      <div class="dactions">
        <button class="btn ghost sm" onclick={() => { store.showFinding(g.flowId, g.items.find((f) => f.flowId === g.flowId) ?? g.items[0]); closeDetail(); }}>Open request in History</button>
        <button class="btn sm" onclick={closeDetail}>Close</button>
      </div>
    </Modal>
  {/if}
</div>

<style>
  .passive {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
  }
  .spacer {
    flex: 1;
  }
  .inscope {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--text-dim);
    font-size: 12px;
    cursor: pointer;
  }
  .inscope input {
    accent-color: var(--accent);
  }
  .btn.danger:not(:disabled) {
    color: var(--red);
  }
  .btn.danger:not(:disabled):hover {
    border-color: var(--red);
    background: color-mix(in srgb, var(--red) 12%, transparent);
  }
  .sevs {
    display: flex;
    gap: 5px;
  }
  .sevchip {
    height: 24px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 600 11px var(--sans);
    text-transform: capitalize;
    cursor: pointer;
  }
  .sevchip:hover {
    color: var(--text);
  }
  .sevchip.on {
    border-color: var(--accent);
    color: var(--accent);
    background: var(--accent-soft);
  }
  .rules {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elev);
  }
  .rules-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    font-size: 12px;
  }
  .cap {
    font: 600 10.5px var(--sans);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .scope {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 5px;
    min-height: 22px;
    align-items: center;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    height: 22px;
    padding: 0 4px 0 9px;
    border: 1px solid var(--border);
    border-radius: 99px;
    background: var(--bg);
    color: var(--text);
    font: 600 11px var(--mono);
  }
  .chip.all {
    padding: 0 10px;
    border-color: var(--accent);
    color: var(--accent);
    background: var(--accent-soft);
  }
  .chipx {
    width: 16px;
    height: 16px;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: none;
    color: var(--text-faint);
    font-size: 13px;
    line-height: 1;
    cursor: pointer;
  }
  .chipx:hover {
    background: var(--bg-hover);
    color: var(--red);
  }
  .scope-add {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .scope-add .ext {
    width: 200px;
    height: 28px;
  }
  .rule-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 160px;
    overflow: auto;
  }
  .rule {
    display: grid;
    grid-template-columns: 18px 62px minmax(90px, 1fr) minmax(140px, 2fr) 24px;
    align-items: center;
    gap: 8px;
    padding: 3px 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    cursor: pointer;
  }
  .rule input {
    accent-color: var(--accent);
  }
  .rule.off {
    opacity: 0.5;
  }
  .rule .tag {
    padding: 0 5px;
    border-radius: 4px;
    background: var(--bg-soft);
    color: var(--text-faint);
    font: 600 9.5px var(--sans);
    text-transform: uppercase;
  }
  .rule .tag.custom {
    color: var(--accent);
  }
  .del-spacer {
    width: 24px;
  }
  .rtitle {
    overflow: hidden;
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rpattern {
    overflow: hidden;
    color: var(--text-dim);
    font-size: 11.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .del {
    width: 22px;
    height: 22px;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font-size: 15px;
    cursor: pointer;
  }
  .del:hover {
    background: var(--bg-hover);
    color: var(--red);
  }
  .rule-add {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .rule-add .input {
    height: 28px;
  }
  .rule-add .title {
    width: 130px;
  }
  .rule-add .sev-sel {
    width: 92px;
  }
  .rule-add .pat {
    flex: 1;
    min-width: 180px;
  }
  .rule-add .flags {
    width: 84px;
  }
  .rule-err {
    margin: 0;
    color: var(--red);
    font: 11.5px var(--mono);
  }
  .progress {
    height: 3px;
    background: var(--bg-soft);
  }
  .progress .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.15s;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 8px 12px;
  }
  .group {
    margin-bottom: 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .grow {
    display: flex;
    align-items: center;
    background: var(--bg-elev);
  }
  .grow .gcheck {
    margin: 0 2px 0 10px;
    accent-color: var(--accent);
    cursor: pointer;
  }
  .caret {
    flex-shrink: 0;
    width: 22px;
    align-self: stretch;
    padding: 0;
    border: 0;
    background: var(--bg-elev);
    color: var(--text-faint);
    font-size: 10px;
    cursor: pointer;
  }
  .caret:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .ghead {
    display: flex;
    align-items: center;
    gap: 10px;
    flex: 1;
    min-width: 0;
    padding: 7px 10px;
    border: 0;
    background: var(--bg-elev);
    color: var(--text);
    text-align: left;
    cursor: pointer;
    font: inherit;
  }
  .ghead:hover {
    background: var(--bg-hover);
  }
  .gsrc {
    flex: 1;
    overflow: hidden;
    font: 12.5px var(--mono);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .gcount {
    flex-shrink: 0;
    color: var(--text-dim);
    font-size: 11.5px;
  }
  .gitems {
    display: flex;
    flex-direction: column;
  }
  .gitem {
    display: grid;
    grid-template-columns: 66px minmax(120px, 1fr) minmax(160px, 2fr) 22px 44px;
    align-items: center;
    gap: 10px;
    padding: 5px 10px 5px 16px;
    border-top: 1px solid var(--border);
    background: var(--bg);
  }
  .gitems.selmode .gitem {
    grid-template-columns: 22px 66px minmax(120px, 1fr) minmax(160px, 2fr) 22px 44px;
    padding-left: 10px;
  }
  .gitem {
    cursor: pointer;
  }
  .gitem:hover {
    background: var(--bg-hover);
  }
  .gitem.sel {
    background: var(--accent-soft);
  }
  .gitem .icheck {
    justify-self: center;
    accent-color: var(--accent);
    cursor: pointer;
  }
  .copy1 {
    width: 20px;
    height: 20px;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: none;
    color: var(--text-faint);
    font-size: 12px;
    cursor: pointer;
    opacity: 0;
  }
  .gitem:hover .copy1 {
    opacity: 1;
  }
  .copy1:hover {
    background: var(--bg-hover);
    color: var(--accent);
  }
  .sev {
    justify-self: start;
    padding: 2px 8px;
    border-radius: 99px;
    font: 700 10px var(--sans);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .sev-high {
    background: color-mix(in srgb, var(--red) 20%, transparent);
    color: var(--red);
  }
  .sev-medium {
    background: color-mix(in srgb, var(--amber) 22%, transparent);
    color: var(--amber);
  }
  .sev-low {
    background: color-mix(in srgb, var(--blue) 20%, transparent);
    color: var(--blue);
  }
  .sev-info {
    background: var(--bg-soft);
    color: var(--text-dim);
  }
  .ftitle {
    font-size: 12.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .match {
    overflow: hidden;
    color: var(--text-dim);
    font-size: 12px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .where {
    display: inline-block;
    margin-right: 4px;
    padding: 0 5px;
    border-radius: 4px;
    background: var(--bg-soft);
    font: 600 10px var(--mono);
  }
  .m {
    font-weight: 700;
  }
  .empty {
    display: grid;
    flex: 1;
    place-content: center;
    gap: 6px;
    padding: 24px;
    text-align: center;
    color: var(--text-dim);
  }
  .empty p {
    margin: 0;
    max-width: 380px;
  }

  /* Finding detail modal */
  .dmeta {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 10px;
  }
  .dsrc {
    overflow: hidden;
    font-size: 12.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dgrid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 12px;
    margin-bottom: 12px;
    font-size: 12.5px;
  }
  .dk {
    color: var(--text-faint);
  }
  .dfindings {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .dfinding {
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 8px 10px;
    background: var(--bg);
  }
  .dfinding.focus {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .dfhead {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }
  .dftitle {
    flex: 1;
    overflow: hidden;
    font-size: 12.5px;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dflow {
    color: var(--text-dim);
    font-size: 11.5px;
  }
  .dmatch {
    margin: 0;
    padding: 8px 10px;
    max-height: 180px;
    overflow: auto;
    border-radius: 6px;
    background: var(--bg-soft);
    color: var(--text);
    font-size: 12px;
    white-space: pre-wrap;
    word-break: break-word;
    user-select: text;
  }
  .dactions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
</style>
