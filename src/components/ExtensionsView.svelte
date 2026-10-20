<script>
  import { store } from '../lib/store.svelte.js';
  import * as api from '../lib/api.js';
  import { pollVisible } from '../lib/poll.js';
  import { open, save } from '@tauri-apps/plugin-dialog';

  const ext = $derived(store.extensions);
  let pythonInput = $state('');
  let venvInput = $state('');
  let pkgInput = $state('');
  let packages = $state([]);
  let logs = $state([]);
  let showSetupLog = $state(false);
  let test = $state(null); // { id, phase, input, output, logs, changed, error, running }

  // Where an extension may run. An extension can cover any combination.
  const SURFACES = [
    { id: 'proxy', short: 'Proxy', title: 'Run on live proxy (History) traffic' },
    { id: 'interceptor', short: 'Intercept', title: 'Run on messages held in the intercept queue' },
    { id: 'repeater', short: 'Repeater', title: 'Run on Repeater sends' },
  ];
  const SAMPLE_REQUEST =
    'POST /login HTTP/1.1\nHost: example.com\nContent-Type: application/json\nContent-Length: 25\n\n{"user":"admin","pw":"x"}';
  const SAMPLE_RESPONSE = 'HTTP/1.1 200 OK\nContent-Type: application/json\n\n{"ok":true,"token":"abc"}';

  // Mirror backend values into the inputs while the user is not editing them.
  $effect(() => {
    if (document.activeElement?.id !== 'py-path') pythonInput = ext.python || '';
  });
  $effect(() => {
    if (document.activeElement?.id !== 'venv-path') venvInput = ext.venv || '';
  });

  // Poll the debug console while this view is open (and the window is visible).
  $effect(() => {
    return pollVisible(async () => {
      try {
        logs = await api.extensionLogs();
      } catch {
        /* ignore */
      }
    }, 1500);
  });
  // Refresh installed packages when the venv gets built.
  $effect(() => {
    ext.venvReady;
    store.listExtensionPackages().then((p) => (packages = p));
  });

  async function addScript() {
    const path = await open({ title: 'Choose a Python extension', filters: [{ name: 'Python', extensions: ['py'] }] });
    if (typeof path === 'string') await store.addExtension({ path });
  }
  async function newFromTemplate() {
    const path = await save({ title: 'New extension from template', defaultPath: 'my_extension.py', filters: [{ name: 'Python', extensions: ['py'] }] });
    if (!path) return;
    const tpl = await store.extensionTemplate();
    await api.saveBinary(path, btoa(unescape(encodeURIComponent(tpl))));
    await store.addExtension({ path });
  }
  function openTest(id) {
    test = { id, phase: 'request', input: SAMPLE_REQUEST, output: '', logs: [], changed: false, error: '', running: false };
  }
  function switchPhase(p) {
    test = { ...test, phase: p, input: p === 'response' ? SAMPLE_RESPONSE : SAMPLE_REQUEST, output: '', logs: [], error: '', changed: false };
  }
  async function runTest() {
    if (!test) return;
    test = { ...test, running: true, error: '', output: '', logs: [] };
    try {
      const r = await store.testExtension(test.id, test.phase, test.input);
      test = { ...test, running: false, output: r.output ?? '', logs: r.logs ?? [], changed: !!r.changed, error: r.error ?? '' };
    } catch (e) {
      test = { ...test, running: false, error: String(e) };
    }
  }
  async function installPkg() {
    if (!pkgInput.trim()) return;
    if (await store.installExtensionPackage(pkgInput)) {
      pkgInput = '';
      packages = await store.listExtensionPackages();
    }
  }
  async function clearLogs() {
    await api.clearExtensionLogs();
    logs = [];
  }
  const testName = $derived(ext.list.find((e) => e.id === test?.id)?.name ?? '');
</script>

<div class="ext">
  <header class="ext-head">
    <div>
      <h1>Extensions</h1>
      <p class="dim">Python scripts that transform requests/responses on the wire. Each script gets an editable message API (see the template); set a Python interpreter, add scripts, test them, then enable.</p>
    </div>
    <label class="master" title="Run enabled extensions on matching traffic">
      <input type="checkbox" checked={ext.enabled} onchange={(e) => store.setExtensionsEnabled(e.currentTarget.checked)} />
      <span>{ext.enabled ? 'On' : 'Off'}</span>
    </label>
  </header>

  <!-- Step 1: interpreter -->
  <section class="card">
    <h2><span class="step">1</span> Python interpreter</h2>
    <div class="row">
      <input id="py-path" class="input mono" bind:value={pythonInput} placeholder="python3 (auto), or /path/to/python" spellcheck="false" />
      <button class="btn sm" onclick={() => store.setPythonPath(pythonInput)}>Save</button>
      <button class="btn ghost sm" onclick={() => store.detectExtensionPython()}>Detect</button>
    </div>
    <p class="dim status">
      {#if ext.python}Using <span class="mono">{ext.python}</span>{:else}<span class="warn">No interpreter set.</span> Set a path or Detect.{/if}
      <span class="hint"> Saved per project.</span>
    </p>
  </section>

  <!-- Step 2: scripts -->
  <section class="card">
    <div class="card-head">
      <h2><span class="step">2</span> Scripts</h2>
      <div class="head-actions">
        <button class="btn ghost sm" onclick={newFromTemplate}>+ New from template…</button>
        <button class="btn sm" onclick={addScript}>+ Add script…</button>
      </div>
    </div>
    {#if !ext.list.length}
      <p class="dim empty">No extensions yet. Start from the template, or add a <span class="mono">.py</span> that defines <span class="mono">process_request</span> / <span class="mono">process_response</span>.</p>
    {:else}
      <div class="table">
        <div class="trow thead">
          <span></span><span>Name</span><span>Scope</span><span>Status</span><span></span>
        </div>
        {#each ext.list as e (e.id)}
          <div class="trow" class:off={!e.enabled}>
            <label class="chk" title="Active"><input type="checkbox" checked={e.enabled} onchange={() => store.toggleExtension(e.id)} /></label>
            <div class="cell-name">
              <span class="name">{e.name}</span>
              <span class="mono path dim" title={e.path}>{e.path || 'saved in project'}</span>
              {#if e.missing}<span class="saved" title="Original file not found; running from the copy saved in this project">saved copy</span>{/if}
            </div>
            <div class="scope" role="group" aria-label="Scope">
              {#each SURFACES as s}
                <button
                  type="button"
                  class="surf"
                  class:on={store.extensionSurfaceOn(e.scope, s.id)}
                  title={s.title}
                  aria-pressed={store.extensionSurfaceOn(e.scope, s.id)}
                  onclick={() => store.toggleExtensionSurface(e.id, s.id)}
                >{s.short}</button>
              {/each}
            </div>
            <span class="badge {e.error ? 'err' : e.running ? 'ok' : 'no'}">{e.error ? 'error' : e.running ? 'running' : 'stopped'}</span>
            <div class="row-actions">
              <button class="btn ghost xs" onclick={() => openTest(e.id)}>Test</button>
              <button class="rm" title="Remove" aria-label="Remove" onclick={() => store.removeExtension(e.id)}>×</button>
            </div>
          </div>
          {#if e.error}<p class="error mono row-err">{e.error}</p>{/if}
        {/each}
      </div>
    {/if}

    {#if test}
      <div class="test">
        <div class="test-head">
          <strong>Test: {testName}</strong>
          <div class="modes">
            <button class="mode" class:active={test.phase === 'request'} onclick={() => switchPhase('request')}>Request</button>
            <button class="mode" class:active={test.phase === 'response'} onclick={() => switchPhase('response')}>Response</button>
          </div>
          <div class="spacer"></div>
          <button class="btn sm" disabled={test.running} onclick={runTest}>{test.running ? 'Running…' : 'Run test'}</button>
          <button class="btn ghost sm" onclick={() => (test = null)}>Close</button>
        </div>
        <div class="test-grid">
          <label class="io">
            <span class="dim">Input ({test.phase})</span>
            <textarea class="input mono" bind:value={test.input} spellcheck="false"></textarea>
          </label>
          <div class="io">
            <span class="dim">Output {#if test.output}<span class="badge {test.changed ? 'ok' : 'no'}">{test.changed ? 'changed' : 'unchanged'}</span>{/if}</span>
            {#if test.error}
              <pre class="out err mono">{test.error}</pre>
            {:else}
              <pre class="out mono">{test.output || 'Run the test to see the transformed message.'}</pre>
            {/if}
          </div>
        </div>
        {#if test.logs?.length}
          <pre class="out logs mono">{test.logs.join('\n')}</pre>
        {/if}
      </div>
    {/if}
  </section>

  <!-- Step 3: venv -->
  <section class="card">
    <h2><span class="step">3</span> Project venv</h2>
    <p class="dim hint">A virtualenv shared by all enabled extensions, saved per project. Default: next to the saved project. Install packages (e.g. <span class="mono">pycryptodome</span>) here.</p>
    <div class="row">
      <input id="venv-path" class="input mono" bind:value={venvInput} placeholder="(default: next to the project)" spellcheck="false" />
      <button class="btn sm" onclick={() => store.setVenvPath(venvInput)}>Save path</button>
      <span class="badge {ext.venvReady ? 'ok' : 'no'}">{ext.venvReady ? 'ready' : 'not built'}</span>
    </div>
    <div class="row">
      <input class="input mono" bind:value={pkgInput} placeholder="package to install, e.g. pycryptodome requests" spellcheck="false"
        onkeydown={(e) => e.key === 'Enter' && installPkg()} />
      <button class="btn sm" disabled={ext.busy || !ext.python} onclick={installPkg}>{ext.busy ? 'Working…' : 'Install'}</button>
      <button class="btn ghost sm" disabled={ext.busy || !ext.python} onclick={() => store.setupExtensionVenv()} title="Create the venv and install every extension's declared requirements">Build venv</button>
      {#if ext.log}<button class="btn ghost sm" onclick={() => (showSetupLog = !showSetupLog)}>{showSetupLog ? 'Hide' : 'Show'} log</button>{/if}
    </div>
    {#if showSetupLog && ext.log}<pre class="out mono setup-log">{ext.log}</pre>{/if}
    <h3>Installed packages ({packages.length})</h3>
    {#if packages.length}
      <div class="pkgs">{#each packages as p}<span class="pkg mono">{p}</span>{/each}</div>
    {:else}
      <p class="dim">{ext.venvReady ? 'None yet.' : 'Build the venv first.'}</p>
    {/if}
  </section>

  <!-- Debug console -->
  <section class="card">
    <div class="card-head"><h2>Debug console</h2><button class="btn ghost sm" onclick={clearLogs}>Clear</button></div>
    <pre class="out console mono">{logs.length ? logs.join('\n') : 'No output yet. Enable an extension and send some traffic, or run a Test.'}</pre>
  </section>
</div>

<style>
  .ext {
    height: 100%;
    overflow: auto;
    padding: 16px 18px;
  }
  .ext-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 12px;
  }
  h1 {
    margin: 0 0 4px;
    font-size: 20px;
  }
  h2 {
    margin: 0;
    font-size: 14px;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  h3 {
    margin: 14px 0 6px;
    font-size: 12.5px;
  }
  .step {
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--accent);
    color: #fff;
    font: 700 11px var(--mono);
  }
  p {
    margin: 0 0 8px;
    font-size: 12.5px;
    line-height: 1.5;
  }
  .master {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 0 0 auto;
    padding: 6px 12px;
    border: 1px solid var(--border);
    border-radius: 99px;
    font-weight: 600;
    font-size: 12px;
    cursor: pointer;
  }
  .master input {
    accent-color: var(--accent);
  }
  .card {
    padding: 14px 16px;
    margin-bottom: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
  }
  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 10px;
  }
  .head-actions {
    display: flex;
    gap: 6px;
  }
  .row {
    display: flex;
    gap: 8px;
    align-items: center;
    margin: 8px 0;
  }
  .row .input {
    flex: 1;
    min-width: 0;
  }
  .status {
    margin: 2px 0 0;
  }
  .warn {
    color: var(--amber);
  }
  .empty {
    color: var(--text-dim);
  }
  .table {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .trow {
    display: grid;
    grid-template-columns: 28px 1fr 210px 78px auto;
    align-items: center;
    gap: 8px;
    padding: 7px 8px;
    border-radius: var(--radius);
  }
  .trow:not(.thead):hover {
    background: var(--bg-soft);
  }
  .thead {
    color: var(--text-dim);
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .trow.off {
    opacity: 0.55;
  }
  .chk input {
    accent-color: var(--accent);
  }
  .cell-name {
    display: grid;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    font-size: 12.5px;
  }
  .cell-name .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
  }
  .saved {
    justify-self: start;
    margin-top: 2px;
    padding: 0 6px;
    border-radius: 99px;
    background: color-mix(in srgb, var(--amber) 20%, transparent);
    color: var(--amber);
    font-size: 10px;
    font-weight: 600;
  }
  .scope {
    display: inline-flex;
    gap: 3px;
    align-items: center;
    flex-wrap: wrap;
  }
  .surf {
    font: inherit;
    font-size: 10.5px;
    line-height: 1;
    padding: 4px 7px;
    border-radius: 5px;
    border: 1px solid var(--border);
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    user-select: none;
  }
  .surf:hover {
    border-color: var(--accent);
  }
  .surf.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .row-actions {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .badge {
    padding: 1px 8px;
    border-radius: 99px;
    font: 600 10.5px var(--mono);
    white-space: nowrap;
  }
  .badge.ok {
    background: color-mix(in srgb, var(--green) 20%, transparent);
    color: var(--green);
  }
  .badge.no {
    background: var(--bg-soft);
    color: var(--text-dim);
  }
  .badge.err {
    background: color-mix(in srgb, var(--red) 20%, transparent);
    color: var(--red);
  }
  .row-err {
    margin: 0 0 6px 36px;
  }
  .rm {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: none;
    color: var(--text-faint);
    font-size: 15px;
    cursor: pointer;
  }
  .rm:hover {
    border-color: var(--red);
    color: var(--red);
  }
  .btn.xs {
    height: 24px;
    padding: 0 8px;
    font-size: 11px;
  }
  .test {
    margin-top: 12px;
    padding: 12px;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--accent) 6%, var(--bg-soft));
  }
  .test-head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
  }
  .modes {
    display: flex;
    gap: 2px;
    padding: 2px;
    border-radius: 7px;
    background: var(--bg-soft);
  }
  .mode {
    height: 24px;
    padding: 0 10px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font: 500 11.5px var(--sans);
    cursor: pointer;
  }
  .mode.active {
    background: var(--bg-elev);
    color: var(--text);
  }
  .spacer {
    flex: 1;
  }
  .test-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .io {
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .io textarea {
    height: 150px;
    resize: vertical;
    line-height: 1.5;
  }
  .out {
    margin: 0;
    max-height: 220px;
    overflow: auto;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg);
    font-size: 11.5px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .out.err {
    color: var(--red);
  }
  .out.logs {
    margin-top: 8px;
    max-height: 120px;
  }
  .setup-log {
    max-height: 200px;
    margin: 4px 0 0;
  }
  .console {
    max-height: 220px;
  }
  .pkgs {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .pkg {
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 99px;
    font-size: 11px;
    background: var(--bg-soft);
  }
</style>
