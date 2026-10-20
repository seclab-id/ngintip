<script>
  import { onMount, untrack } from 'svelte';
  import { store, DETAIL_CACHE_MAX } from '../lib/store.svelte.js';
  import * as api from '../lib/api.js';
  import { pollVisible } from '../lib/poll.js';
  import { isLoopback } from '../lib/net.js';
  import { BRAND } from '../lib/brand.js';
  import SitemapTree from './SitemapTree.svelte';

  // Sitemap under Scope: browse discovered hosts and click one to add it to scope.
  let sitemapSel = $state(null);
  function addHostFromNode(key) {
    const host = String(key || '').replace(/^https?:\/\//, '').split('/')[0].replace(/:\d+$/, '');
    if (!host) return;
    const lines = scopeText.split('\n').map((l) => l.trim()).filter(Boolean);
    if (!lines.includes(host)) scopeText = [...lines, host].join('\n');
  }
  $effect(() => {
    if (sitemapSel) {
      addHostFromNode(sitemapSel);
      sitemapSel = null;
    }
  });
  import { save } from '@tauri-apps/plugin-dialog';

  // Usage panel: poll process/system stats while its tab is open. No blocking.
  let usage = $state(null);
  let crash = $state(null);
  const fmtBytes = (n) => {
    if (!n && n !== 0) return '-';
    const u = ['B', 'KB', 'MB', 'GB', 'TB'];
    let i = 0;
    let v = n;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(v >= 100 || i === 0 ? 0 : 1)} ${u[i]}`;
  };
  const fmtDuration = (s) => {
    s = Math.max(0, Math.floor(s || 0));
    const d = Math.floor(s / 86400);
    const h = Math.floor((s % 86400) / 3600);
    const m = Math.floor((s % 3600) / 60);
    const sec = s % 60;
    return [d && `${d}d`, (d || h) && `${h}h`, `${m}m`, `${sec}s`].filter(Boolean).join(' ');
  };
  async function pollUsage() {
    try {
      usage = await api.usageStats();
    } catch {
      /* ignore transient errors */
    }
  }
  $effect(() => {
    if (section !== 'project') return;
    api.crashInfo().then((c) => (crash = c)).catch(() => {});
    return pollVisible(pollUsage, 1500);
  });

  // Settings is split into sub-tabs; one section shows at a time, full-width.
  const SECTIONS = [
    { id: 'project', label: 'Project' },
    { id: 'autosave', label: 'Auto Saved' },
    { id: 'scope', label: 'Scope' },
    { id: 'rules', label: 'Match & Replace' },
    { id: 'listener', label: 'Proxy listener' },
    { id: 'mcp', label: 'MCP server' },
    { id: 'browser', label: 'Browser Engine' },
    { id: 'ca', label: 'CA certificate' },
  ];
  let section = $state('project');
  // Let other views deep-link to a section (e.g. History's "Open browser").
  $effect(() => {
    if (store.settingsSection) {
      section = store.settingsSection;
      store.settingsSection = null;
    }
  });

  // Browser Engine tab local state.
  let browserPath = $state(store.browser.path);
  let browserFlags = $state(store.browser.flags);
  $effect(() => {
    // Refresh the fields when a detect/download fills them in.
    browserPath = store.browser.path;
    browserFlags = store.browser.flags;
  });

  let host = $state(store.proxy.host);
  let port = $state(store.proxy.port);
  let ca = $state(null);
  let copied = $state('');

  let interfaces = $state([]);
  let scopeText = $state(store.scope.join('\n'));
  let scopeDirty = $derived(scopeText.split('\n').map((l) => l.trim().toLowerCase()).filter(Boolean).join('\n') !== store.scope.join('\n'));

  async function saveScope() {
    await store.saveScope(scopeText.split('\n'));
    scopeText = store.scope.join('\n');
  }

  // Delete History flows that fall outside the saved scope. Count only computed while the
  // Scope section is open so it never re-filters the whole history during capture.
  let oosConfirm = $state(false);
  let oosBusy = $state(false);
  let oosResetId = $state(false);
  let outOfScopeCount = $derived(section === 'scope' ? store.outOfScopeCount() : 0);
  async function deleteOutOfScope() {
    oosBusy = true;
    try {
      await store.deleteOutOfScope(oosResetId);
    } finally {
      oosBusy = false;
      oosConfirm = false;
    }
  }

  // Performance knobs (Settings > Project).
  let threadsInput = $state(store.config.threads ?? 0);
  let cacheInput = $state(store.detailCacheMax);
  $effect(() => {
    const n = store.config.threads ?? 0;
    untrack(() => (threadsInput = n));
  });

  const TARGETS = [
    { id: 'requestLine', label: 'Request line' },
    { id: 'requestHeader', label: 'Request header' },
    { id: 'requestBody', label: 'Request body' },
    { id: 'responseHeader', label: 'Response header' },
    { id: 'responseBody', label: 'Response body' },
  ];
  const clone = (v) => JSON.parse(JSON.stringify(v));
  let rules = $state(clone(store.rules));
  let rulesError = $state('');
  let rulesDirty = $derived(JSON.stringify(rules) !== JSON.stringify(store.rules));

  // Another project opened (or rules saved): show what's active now.
  $effect(() => {
    const active = store.rules;
    untrack(() => (rules = clone(active)));
  });
  $effect(() => {
    const active = store.scope;
    untrack(() => (scopeText = active.join('\n')));
  });

  function addRule() {
    rules.push({ enabled: true, target: 'requestHeader', match: '', replace: '', comment: '' });
  }

  let testOpen = $state(false);
  let testResponse = $state(false);
  let testSample = $state('');
  let testOutput = $state('');
  let testError = $state('');

  async function runTest() {
    testError = '';
    testOutput = '';
    try {
      testOutput = await api.testRules(clone(rules), testSample, testResponse);
    } catch (e) {
      testError = String(e);
    }
  }

  async function saveRules() {
    rulesError = '';
    try {
      await store.saveRules(clone(rules));
    } catch (e) {
      rulesError = String(e);
    }
  }
  // "Custom" lets the user type any address, e.g. an IP not assigned yet.
  let custom = $state(false);

  let auth = $state({ ...store.config.auth });
  let authError = $state('');
  let authSaved = $derived(JSON.stringify(auth) === JSON.stringify(store.config.auth));

  async function saveAuth() {
    authError = '';
    try {
      await store.saveAuth(auth);
    } catch (e) {
      authError = String(e);
    }
  }

  let upstream = $state(store.config.upstream);
  let upstreamError = $state('');

  async function saveUpstream(url) {
    upstreamError = '';
    try {
      await store.saveUpstream(url);
      upstream = store.config.upstream;
    } catch (e) {
      upstreamError = String(e);
    }
  }

  // Warn about the address being configured and the one actually in use.
  let exposed = $derived(!isLoopback(host) || store.exposed);

  onMount(async () => {
    [ca, interfaces] = await Promise.all([api.caInfo(), api.listInterfaces()]);
    custom = !interfaces.some((i) => i.ip === host);
  });

  function pickInterface(e) {
    if (e.currentTarget.value === '__custom') {
      custom = true;
    } else {
      custom = false;
      host = e.currentTarget.value;
    }
  }

  // Addresses other devices can use when listening on every interface.
  let reachable = $derived(
    store.proxy.running && store.proxy.host === '0.0.0.0'
      ? interfaces.filter((i) => !['0.0.0.0', '127.0.0.1'].includes(i.ip))
      : [],
  );

  async function copy(label, text) {
    await navigator.clipboard.writeText(text);
    copied = label;
    setTimeout(() => (copied = ''), 1200);
  }

  // MCP server (lets an LLM drive the app over HTTP).
  let mcpPort = $state(store.mcp.port);
  let mcpDocs = $state(store.mcp.docsPath);
  let mcpError = $state('');
  async function toggleMcp(enabled) {
    mcpError = '';
    try {
      await store.setMcp(enabled, Number(mcpPort) || 8082, mcpDocs || '/docs');
      mcpPort = store.mcp.port;
      mcpDocs = store.mcp.docsPath;
    } catch (e) {
      mcpError = String(e);
    }
  }
  // The proxy's own CA download page (served on the listener); 0.0.0.0 -> localhost.
  let caPageUrl = $derived(`http://${store.proxy.host === '0.0.0.0' ? '127.0.0.1' : store.proxy.host}:${store.proxy.port}/`);
  async function openCaPage() {
    try {
      await api.openUrl(caPageUrl);
    } catch (e) {
      store.notice = { text: String(e), error: true };
      setTimeout(() => (store.notice = null), 2500);
    }
  }

  let regenConfirm = $state(false);
  let regenBusy = $state(false);
  async function exportCa() {
    const path = await save({
      title: 'Export CA certificate',
      defaultPath: `${BRAND.id}-ca.crt`,
      filters: [
        { name: 'DER certificate (Android/Windows)', extensions: ['crt', 'der', 'cer'] },
        { name: 'PEM certificate', extensions: ['pem'] },
      ],
    });
    if (!path) return;
    try {
      await api.exportCa(path);
      store.notice = { text: 'CA exported', error: false };
      setTimeout(() => (store.notice = null), 2500);
    } catch (e) {
      store.notice = { text: String(e), error: true };
    }
  }

  async function regenerateCa() {
    regenBusy = true;
    try {
      ca = await api.regenerateCa();
      store.notice = { text: 'New CA generated. Re-install and trust it.', error: false };
      setTimeout(() => (store.notice = null), 3500);
    } catch (e) {
      store.notice = { text: String(e), error: true };
    } finally {
      regenBusy = false;
      regenConfirm = false;
      copied = '';
    }
  }

  // CA install steps per platform; starts on the one Cegat runs on.
  const PLATFORMS = ['macOS', 'Windows', 'Linux', 'Firefox', 'Burp Suite', 'Android / iOS'];
  const ua = navigator.userAgent;
  let platform = $state(/Windows/.test(ua) ? 'Windows' : /Linux/.test(ua) ? 'Linux' : 'macOS');
  let proxyUrl = $derived(`http://${store.proxy.host === '0.0.0.0' ? (reachable[0]?.ip ?? '<ip-laptop>') : store.proxy.host}:${store.proxy.port}/`);
  let steps = $derived.by(() => {
    if (!ca) return { cmds: [], note: '' };
    const p = ca.path;
    switch (platform) {
      case 'macOS':
        return {
          cmds: [{ label: 'System keychain', cmd: `sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain "${p}"` }],
          note: 'Safari, Chrome and other macOS apps use this keychain.',
        };
      case 'Windows':
        return {
          cmds: [
            { label: 'Current user', cmd: `certutil -user -addstore Root "${p}"` },
            { label: 'All users (admin)', cmd: `certutil -addstore -f Root "${p}"` },
          ],
          note: 'Run in Command Prompt or PowerShell. Edge and Chrome use the Windows trust store.',
        };
      case 'Linux':
        return {
          cmds: [
            { label: 'Debian/Ubuntu', cmd: `sudo cp "${p}" /usr/local/share/ca-certificates/${BRAND.id}.crt && sudo update-ca-certificates` },
            { label: 'Fedora/Arch', cmd: `sudo trust anchor --store "${p}"` },
            { label: 'Chrome (NSS)', cmd: `certutil -d sql:$HOME/.pki/nssdb -A -t "C,," -n ${BRAND.name} -i "${p}"` },
          ],
          note: 'Chrome/Chromium on Linux use their own NSS database (libnss3-tools package).',
        };
      case 'Firefox':
        return { cmds: [], note: 'Firefox uses its own trust store: Settings > Privacy & Security > Certificates > View Certificates > Authorities > Import, then pick the CA file above and check "trust to identify websites".' };
      case 'Burp Suite':
        return {
          cmds: [{ label: 'Trust in Burp JRE (keytool)', cmd: `sudo keytool -importcert -noprompt -alias ${BRAND.id} -file "${p}" -keystore "$JAVA_HOME/lib/security/cacerts" -storepass changeit` }],
          note: `To chain Burp through ${BRAND.name}, set ${BRAND.name} as Burp's upstream proxy: Burp > Settings > Network > Connections > Upstream proxy servers > Add, destination host *, proxy host 127.0.0.1, port ${store.proxy.port}. Trust ${BRAND.name}'s CA in the browser as usual. The keytool command makes Burp's own JRE trust ${BRAND.name} when ${BRAND.name} sits upstream of Burp.`,
        };
      default:
        return {
          cmds: [{ label: 'Open on the device', cmd: proxyUrl }],
          note: `Set the device proxy to ${BRAND.name} (listener must be 0.0.0.0 or a LAN IP), open this URL, then install the .cer file. Android: Settings > Security > Install a certificate > CA certificate (apps only trust it if they opt in to user CAs). iOS: install the profile, then enable it under General > About > Certificate Trust Settings.`,
        };
    }
  });
</script>

{#snippet copyIcon(key, text)}
  <button class="btn ghost icon-btn" title="Copy to clipboard" aria-label="Copy to clipboard" onclick={() => copy(key, text)}>
    {#if copied === key}
      <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5" /></svg>
    {:else}
      <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="11" height="11" rx="2" /><path d="M5 15V5a2 2 0 0 1 2-2h10" /></svg>
    {/if}
  </button>
{/snippet}

<div class="settings">
  <div class="sub-nav" role="tablist" aria-label="Settings sections">
    {#each SECTIONS as s}
      <button class="sub-nav-item" class:active={section === s.id} role="tab" aria-selected={section === s.id} onclick={() => (section = s.id)}>
        <span>{s.label}</span>
        {#if s.id === 'listener' && store.proxy.running}<span class="dot on" title="Running"></span>{/if}
        {#if s.id === 'rules' && store.rules.filter((r) => r.enabled).length}<span class="ncount">{store.rules.filter((r) => r.enabled).length}</span>{/if}
        {#if s.id === 'scope' && store.scope.length}<span class="ncount">{store.scope.length}</span>{/if}
      </button>
    {/each}
  </div>

  <div class="content">
  {#if section === 'project'}
  <section class="card">
    <h2>Project</h2>
    <p class="dim">History is saved automatically to a project file (SQLite). One <span class="mono">.{BRAND.ext}</span> file = one project.</p>
    <div class="kv">
      <span class="dim">Active</span>
      <span class="mono path" title={store.project.path}>{store.project.path}</span>
      <button class="btn ghost sm" onclick={() => copy('project', store.project.path)}>{copied === 'project' ? 'Copied ✓' : 'Copy path'}</button>
    </div>
    <div class="actions">
      <button class="btn sm" onclick={() => store.newProject()}>New project…</button>
      <button class="btn sm" onclick={() => store.openProject()}>Open project…</button>
      <button class="btn sm" onclick={() => store.saveProjectAs()}>Save as…</button>
    </div>
  </section>

  <section class="card">
    <h2>Performance</h2>
    <p class="dim">Tuning for this machine, saved locally.</p>
    <div class="form">
      <label>
        <span>Worker threads</span>
        <input class="input mono" type="number" min="0" max="256" bind:value={threadsInput} onchange={() => store.setThreads(threadsInput)} />
      </label>
      <label>
        <span>Detail cache (flows)</span>
        <input class="input mono" type="number" min="5" max="2000" bind:value={cacheInput} onchange={() => store.setDetailCacheMax(cacheInput)} />
      </label>
    </div>
    <p class="dim hint-line">
      <strong>Worker threads:</strong> async runtime threads for the proxy. <span class="mono">0</span> = Auto (one per CPU core, recommended). Takes effect after an app restart.
      <br />
      <strong>Detail cache:</strong> how many opened requests stay in memory for instant re-open (default {DETAIL_CACHE_MAX}). Higher = snappier revisits but more RAM. Applies right away.
    </p>
  </section>
  {/if}

  {#if section === 'autosave'}
  <section class="card">
    <h2>Auto Saved</h2>
    <p class="dim">
      Automatically save the project to its <span class="mono">.{BRAND.ext}</span> file on a timer. Each save shows the same
      freeze overlay as a manual save. These settings are stored with the project.
    </p>
    <div class="form">
      <label class="check">
        <input type="checkbox" checked={store.autoSave.enabled} onchange={(e) => store.setAutoSave({ enabled: e.currentTarget.checked })} />
        Enable auto-save
      </label>
      <label>
        <span>Every</span>
        <input
          class="input mono"
          type="number"
          min="5"
          step="5"
          placeholder="3600"
          value={store.autoSave.interval}
          onchange={(e) => store.setAutoSave({ interval: e.currentTarget.value })}
          disabled={!store.autoSave.enabled}
        />
      </label>
      <span class="dim">seconds</span>
    </div>
    {#if store.autoSave.enabled && !store.project.path}
      <p class="error">Save the project to a file first (Project → Save as…) — auto-save won't prompt for a location.</p>
    {:else if store.autoSave.enabled}
      <p class="dim status-line">Saving every {store.autoSave.interval}s to <span class="mono path">{store.project.path}</span>.</p>
    {:else}
      <p class="dim status-line">Off.</p>
    {/if}
  </section>
  {/if}

  {#if section === 'scope'}
  <section class="card">
    <h2>Scope</h2>
    <p class="dim">
      One host per line. <span class="mono">*.example.com</span> = that domain plus all subdomains. Empty = everything in scope.
      Intercept only holds in-scope requests; History can be filtered by "In scope".
    </p>
    <textarea class="input mono scope" bind:value={scopeText} placeholder="example.com&#10;*.api.example.com" spellcheck="false"></textarea>
    <div class="actions">
      <button class="btn primary sm" onclick={saveScope} disabled={!scopeDirty}>Save scope</button>
      <span class="dim count">{store.scope.length ? `${store.scope.length} patterns active` : 'All hosts in scope'}</span>
    </div>

    {#if store.scope.length}
      <div class="actions oos">
        {#if oosConfirm}
          <button class="btn sm danger" onclick={deleteOutOfScope} disabled={oosBusy || !outOfScopeCount}>{oosBusy ? 'Deleting…' : `Confirm delete ${outOfScopeCount}`}</button>
          <button class="btn ghost sm" onclick={() => (oosConfirm = false)} disabled={oosBusy}>Cancel</button>
          <span class="dim">Permanently removes out-of-scope requests from History (kept data stays).</span>
        {:else}
          <button
            class="btn ghost sm danger"
            onclick={() => (oosConfirm = true)}
            disabled={!outOfScopeCount}
            title="Delete every captured request whose host is not in the scope above"
          >Delete out-of-scope flows{outOfScopeCount ? ` (${outOfScopeCount})` : ''}</button>
          <label class="check" title="After deleting, reseed flow-id numbering so new ids stay compact">
            <input type="checkbox" bind:checked={oosResetId} /> Reset flow ids after delete
          </label>
          {#if scopeDirty && outOfScopeCount}<span class="dim">Based on the saved scope; Save to apply edits.</span>{/if}
        {/if}
      </div>
    {/if}

    <div class="sitemap-head">
      <span class="cap">Sitemap</span>
      <span class="dim">Discovered hosts and paths. Click a host to add it to the scope above, then Save.</span>
    </div>
    {#if store.flows.length}
      <div class="sitemap-box"><SitemapTree flows={store.flows} bind:selected={sitemapSel} oncontext={() => {}} /></div>
    {:else}
      <p class="dim">No requests captured yet.</p>
    {/if}
  </section>
  {/if}

  {#if section === 'rules'}
  <section class="card">
    <h2>Match &amp; Replace</h2>
    <p class="dim">
      Regex (Rust syntax) applied automatically to proxy traffic, before intercept. Headers are matched per
      <span class="mono">Name: value</span> line; an empty match adds a header, an empty result removes it.
      <span class="mono">$1</span> / <span class="mono">${'{name}'}</span> for groups. Compressed bodies are decoded first.
    </p>
    {#if rules.length}
      <div class="rules">
        {#each rules as r, i}
          <div class="rule" class:off={!r.enabled}>
            <input type="checkbox" bind:checked={r.enabled} title="Enabled" aria-label="Enabled" />
            <select class="input compact" bind:value={r.target} aria-label="Target">
              {#each TARGETS as t}<option value={t.id}>{t.label}</option>{/each}
            </select>
            <input class="input mono" bind:value={r.match} placeholder={r.target.endsWith('Header') ? '(empty = add header)' : 'regex'} spellcheck="false" aria-label="Match" />
            <input class="input mono" bind:value={r.replace} placeholder="replace with" spellcheck="false" aria-label="Replace" />
            <input class="input" bind:value={r.comment} placeholder="note" spellcheck="false" aria-label="Note" />
            <button class="btn ghost sm" title="Remove rule" aria-label="Remove rule" onclick={() => rules.splice(i, 1)}>×</button>
          </div>
        {/each}
      </div>
    {/if}
    <div class="actions">
      <button class="btn sm" onclick={addRule}>+ Add rule</button>
      <button class="btn primary sm" onclick={saveRules} disabled={!rulesDirty}>Save</button>
      <button class="btn sm" onclick={() => (testOpen = !testOpen)}>{testOpen ? 'Hide test' : 'Test rules'}</button>
      <span class="dim count">
        {store.rules.filter((r) => r.enabled).length ? `${store.rules.filter((r) => r.enabled).length} rules active` : 'No rules active'}
      </span>
      {#if rulesError}<span class="error inline">{rulesError}</span>{/if}
    </div>

    {#if testOpen}
      <div class="tester">
        <p class="dim">Paste a sample and apply the rules above (edited or saved, without touching live traffic).</p>
        <div class="tester-controls">
          <label class="radio"><input type="radio" bind:group={testResponse} value={false} /> Request</label>
          <label class="radio"><input type="radio" bind:group={testResponse} value={true} /> Response</label>
          <button class="btn primary sm" onclick={runTest}>Run</button>
          {#if testError}<span class="error inline">{testError}</span>{/if}
        </div>
        <div class="tester-grid">
          <label class="tester-pane">
            <span class="dim">Sample {testResponse ? 'response' : 'request'}</span>
            <textarea class="input mono" bind:value={testSample} spellcheck="false"
              placeholder={testResponse ? 'HTTP/1.1 200 OK\nContent-Type: application/json\n\n{"role":"user"}' : 'GET /path HTTP/1.1\nHost: example.com\n\n'}></textarea>
          </label>
          <label class="tester-pane">
            <span class="dim">After rules</span>
            <textarea class="input mono" value={testOutput} readonly spellcheck="false" placeholder="Result appears here"></textarea>
          </label>
        </div>
      </div>
    {/if}
  </section>
  {/if}

  {#if section === 'listener'}
  <section class="card">
    <h2>Proxy listener</h2>
    <p class="dim">Pick the interface the proxy listens on, or Custom to type an IP. <span class="mono">0.0.0.0</span> = all interfaces (for testing from a phone or other device).</p>
    <div class="form">
      <label>
        <span>Interface</span>
        <select class="input select" value={custom ? '__custom' : host} onchange={pickInterface}>
          {#each interfaces as i}
            <option value={i.ip}>{i.ip} - {i.name}</option>
          {/each}
          <option value="__custom">Custom…</option>
        </select>
      </label>
      {#if custom}
        <label>
          <span>Host / IP</span>
          <input class="input mono" bind:value={host} placeholder="192.168.1.10" spellcheck="false" />
        </label>
      {/if}
      <label>
        <span>Port</span>
        <input class="input mono" type="number" min="1" max="65535" bind:value={port} />
      </label>
      <button class="btn primary" onclick={() => store.startProxy(host, port)}>
        {store.proxy.running ? 'Restart' : 'Start'}
      </button>
    </div>
    {#if store.proxy.running}
      <p class="dim status-line">
        Listening on <span class="mono">{store.proxy.host}:{store.proxy.port}</span>
        {@render copyIcon('proxyaddr', `${store.proxy.host}:${store.proxy.port}`)}
      </p>
    {/if}
    {#if store.proxyError}<p class="error">{store.proxyError}</p>{/if}
    {#if exposed}
      <div class="warn" class:danger={!store.config.auth.enabled}>
        <strong>The proxy is reachable from the network.</strong>
        Anyone who can connect to this address can use the proxy, and their traffic is recorded in History too.
        {#if store.config.auth.enabled}Authentication is on.{:else}Turn on authentication below, or use 127.0.0.1.{/if}
      </div>
    {/if}
    {#if reachable.length}
      <p class="dim reach">
        Other devices on the network can use:
        {#each reachable as i, n}<span class="mono">{i.ip}:{store.proxy.port}</span>{n < reachable.length - 1 ? ', ' : ''}{/each}
      </p>
    {/if}
    <label class="check invisible">
      <input type="checkbox" checked={store.config.invisible} onchange={(e) => store.setInvisible(e.currentTarget.checked)} />
      Invisible proxy
    </label>
    <p class="dim hint-line">
      For non-proxy-aware clients (thick clients, Flutter apps) redirected here (via hosts file or firewall). Direct
      connections are routed by TLS SNI or the Host header instead of needing CONNECT. TLS targets are assumed on port 443.
    </p>
  </section>
  {/if}

  {#if section === 'listener'}
  <section class="card">
    <h2>Upstream proxy</h2>
    <p class="dim">
      Forward all traffic through another proxy. Use <span class="mono">http://</span> or <span class="mono">socks5://</span> (Tor, corporate proxy, another tool).
      Leave empty to connect directly.
    </p>
    <div class="form">
      <label class="grow">
        <span>URL</span>
        <input
          class="input mono"
          bind:value={upstream}
          placeholder="socks5://127.0.0.1:9050 or http://user:pass@proxy:3128"
          spellcheck="false"
          onkeydown={(e) => e.key === 'Enter' && saveUpstream(upstream)}
        />
      </label>
      <button class="btn primary" onclick={() => saveUpstream(upstream)} disabled={upstream.trim() === store.config.upstream}>Save</button>
      {#if store.config.upstream}
        <button class="btn" onclick={() => saveUpstream('')}>Clear</button>
      {/if}
    </div>
    {#if upstreamError}
      <p class="error">{upstreamError}</p>
    {:else}
      <p class="dim status-line">
        {store.config.upstream ? `Active: all requests via ${store.config.upstream.replace(/^([a-z]+:\/\/)?[^@/]*@/i, '$1•••@')}` : 'Direct (no upstream proxy)'}
      </p>
    {/if}
  </section>
  {/if}

  {#if section === 'listener'}
  <section class="card">
    <h2>Proxy authentication</h2>
    <p class="dim">
      Clients must send <span class="mono">Proxy-Authorization: Basic</span> (the browser will prompt for login). This header is not forwarded
      to the target and not stored in History. The CA download page stays open.
    </p>
    <label class="check">
      <input type="checkbox" bind:checked={auth.enabled} />
      Require username &amp; password
    </label>
    {#if auth.enabled}
      <div class="form">
        <label>
          <span>Username</span>
          <input class="input mono" bind:value={auth.username} autocomplete="off" spellcheck="false" />
        </label>
        <label>
          <span>Password</span>
          <input class="input mono" type="password" bind:value={auth.password} autocomplete="new-password" />
        </label>
      </div>
    {/if}
    <div class="actions">
      <button class="btn primary sm" onclick={saveAuth} disabled={authSaved}>Save</button>
      {#if authError}<span class="error inline">{authError}</span>{/if}
    </div>
  </section>
  {/if}

  {#if section === 'mcp'}
  <section class="card">
    <h2>MCP server</h2>
    <p class="dim">
      Expose {BRAND.name} to an LLM over the Model Context Protocol (Streamable HTTP), bound to
      <span class="mono">127.0.0.1</span> only. The assistant can read/search history, send to Repeater
      (recorded), fire parallel batches (race conditions), run Intruder attacks, and view/trigger/configure
      the passive scan.
    </p>
    <div class="form">
      <label class="check">
        <input type="checkbox" checked={store.mcp.running} onchange={(e) => toggleMcp(e.currentTarget.checked)} />
        Enable MCP server
      </label>
      <label>
        <span>Port</span>
        <input class="input mono" type="number" min="1" max="65535" bind:value={mcpPort} onchange={() => store.mcp.running && toggleMcp(true)} />
      </label>
      <label>
        <span>Docs path</span>
        <input class="input mono" bind:value={mcpDocs} placeholder="/docs" spellcheck="false" onchange={() => store.mcp.running && toggleMcp(true)} />
      </label>
    </div>
    {#if mcpError}
      <p class="error">{mcpError}</p>
    {:else if store.mcp.running}
      <p class="dim status-line">
        Endpoint <span class="mono">http://127.0.0.1:{store.mcp.port}/mcp</span>
        {@render copyIcon('mcp', `http://127.0.0.1:${store.mcp.port}/mcp`)}
        · docs <span class="mono">http://127.0.0.1:{store.mcp.port}{store.mcp.docsPath}</span>
        {@render copyIcon('mcpdocs', `http://127.0.0.1:${store.mcp.port}${store.mcp.docsPath}`)}
      </p>
    {:else}
      <p class="dim status-line">Off.</p>
    {/if}
    <p class="dim hint-line">
      Point an MCP client (e.g. Claude Desktop, or any HTTP JSON-RPC client) at the endpoint. Tools:
      <span class="mono">history_list/get/search</span>, <span class="mono">history_set_note/set_color/get_color</span>,
      <span class="mono">repeater_send</span>, <span class="mono">repeater_send_batch</span>, <span class="mono">repeater_groups/send_group</span>,
      <span class="mono">intruder_run</span>, <span class="mono">passive_findings/scan/rules_get/rules_set</span>.
    </p>
  </section>
  {/if}

  {#if section === 'browser'}
  <section class="card">
    <h2>Browser Engine</h2>
    <p class="dim">
      A Chromium/Chrome binary the History <b>Open browser</b> button launches, pre-wired to this proxy
      (fresh profile, proxy set, MITM certs accepted) so everything you browse is recorded. Point it at an
      installed browser, or download a self-contained Chrome for Testing.
    </p>
    <div class="bform">
      <label>
        <span>Browser path</span>
        <input class="input mono" bind:value={browserPath} placeholder="/path/to/chrome (or a .app on macOS)" spellcheck="false" />
      </label>
      <label>
        <span>Extra flags</span>
        <input class="input mono" bind:value={browserFlags} placeholder="--incognito --lang=en (optional)" spellcheck="false" />
      </label>
      <div class="btn-row">
        <button class="btn sm" onclick={() => store.setBrowserPath(browserPath, browserFlags)}>Save</button>
        <button class="btn ghost sm" onclick={() => store.detectBrowser()}>Detect installed</button>
        <button class="btn ghost sm" disabled={!store.browser.canDownload || store.browser.downloading} onclick={() => store.downloadBrowser()}>
          {store.browser.downloading ? 'Downloading…' : 'Download Chrome for Testing'}
        </button>
      </div>

      <div class="cdp">
        <label class="cdp-row">
          <input type="checkbox" checked={store.cdp.enabled} onchange={(e) => store.setCdp({ enabled: e.currentTarget.checked })} />
          <span>Enable CDP (Chrome DevTools Protocol)</span>
        </label>
        <label class="cdp-port">
          <span class="dim">Port</span>
          <input
            class="input mono port"
            type="number"
            min="1"
            max="65535"
            value={store.cdp.port}
            disabled={!store.cdp.enabled}
            onchange={(e) => store.setCdp({ port: Number(e.currentTarget.value) })}
          />
        </label>
        <p class="dim hint">
          Launches the browser with <span class="mono">--remote-debugging-port</span> on 127.0.0.1 so tools can attach.
          Saved with this project; default off.
        </p>
      </div>

      <div class="cdp profiles">
        <label class="cdp-row">
          <input type="checkbox" checked={store.profiles.persist} onchange={(e) => store.setProfiles({ persist: e.currentTarget.checked })} />
          <span>Save browser profile (cookies &amp; history) with this project</span>
        </label>
        <p class="dim hint">
          Keeps the default profile at <span class="mono">{BRAND.id}_browser/{store.project.name || 'untitled'}_default/</span>
          next to the saved <span class="mono">.{BRAND.ext}</span>, so a return visit stays logged in. Off = a throwaway profile each time.
        </p>

        <label class="cdp-row">
          <input type="checkbox" checked={store.profiles.multi} onchange={(e) => store.setProfiles({ multi: e.currentTarget.checked })} />
          <span>Multiple profiles (spawn several isolated browsers)</span>
        </label>
        {#if store.profiles.multi}
          <div class="prof-list">
            {#each store.profiles.list as p, i (i)}
              <div class="prof-row">
                <input class="input mono" value={p} onchange={(e) => store.renameProfile(i, e.currentTarget.value)} placeholder="profile name" spellcheck="false" />
                <button class="btn sm" disabled={!store.browser.exists} title="Open a browser on this profile" onclick={() => store.spawnBrowserProfile(p)}>Spawn</button>
                <button class="btn ghost sm" title="Remove profile" aria-label="Remove profile" onclick={() => store.removeProfile(i)}>×</button>
              </div>
            {/each}
            <button class="btn ghost sm add-prof" onclick={() => store.addProfile()}>+ Add profile</button>
          </div>
          <p class="dim hint">
            Each profile is its own folder <span class="mono">{store.project.name || 'untitled'}_&lt;name&gt;</span> with separate
            cookies - handy for multi-account testing. Spawn opens a browser wired to the proxy on that profile.
          </p>
        {/if}
        {#if (store.profiles.persist || store.profiles.multi) && !store.project.path}
          <p class="error">Save the project first (Project &rarr; Save as) so profiles live next to the <span class="mono">.{BRAND.ext}</span>; until then they go under the app data folder.</p>
        {/if}
      </div>
    </div>

    {#if store.browser.downloading}
      <div class="progress"><div class="fill" style="width: {store.browser.downloadPct ?? 0}%"></div></div>
      <p class="dim status-line">Downloading… {store.browser.downloadPct ?? 0}% (this is a large one-time download).</p>
    {:else if store.browser.downloadError}
      <p class="error">{store.browser.downloadError}</p>
    {:else if store.browser.exists}
      <p class="dim status-line">Ready: <span class="mono">{store.browser.path}</span></p>
    {:else if store.browser.path}
      <p class="error">Not found at that path.</p>
    {:else}
      <p class="dim status-line">No browser set. Detect an installed one or download Chrome for Testing.</p>
    {/if}
    {#if !store.browser.canDownload}
      <p class="dim hint-line">Auto-download is not available on this OS/arch; set a path to an installed browser.</p>
    {/if}
    <p class="dim hint-line">
      For a warning-free session, trust the CA (see the CA certificate tab). The launcher passes
      <span class="mono">--ignore-certificate-errors</span> in a throwaway profile so it also works without trusting it.
    </p>
  </section>
  {/if}

  {#if section === 'listener'}
  <section class="card">
    <h2>Sending</h2>
    <p class="dim">
      How Repeater and Intruder build requests before sending.
    </p>
    <label class="check">
      <input type="checkbox" checked={store.config.autoContentLength} onchange={(e) => store.setAutoContentLength(e.currentTarget.checked)} />
      Auto add / recompute Content-Length
    </label>
    <p class="dim hint-line">
      On (default): a request with a body always gets a correct <span class="mono">Content-Length</span>. Turn off to send the
      <span class="mono">Content-Length</span> exactly as written (e.g. request smuggling / desync testing).
    </p>
  </section>
  {/if}

  {#if section === 'ca'}
  <section class="card">
    <h2>CA certificate</h2>
    <p class="dim">
      Install &amp; trust this CA so HTTPS traffic can be intercepted. "Open in browser" opens the proxy's download
      page (<span class="mono">{caPageUrl}</span>) in your default browser, where you can download and install it.
    </p>
    {#if ca}
      <div class="kv">
        <span class="dim">File</span>
        <span class="mono path">{ca.path}</span>
        <button class="btn ghost sm" onclick={() => copy('path', ca.path)}>{copied === 'path' ? 'Copied ✓' : 'Copy path'}</button>
      </div>
      <div class="platforms" role="tablist">
        {#each PLATFORMS as p}
          <button class="plat" class:active={platform === p} role="tab" aria-selected={platform === p} onclick={() => (platform = p)}>{p}</button>
        {/each}
      </div>
      {#each steps.cmds as step, i}
        <div class="kv wide">
          <span class="dim">{step.label}</span>
          <code class="mono path" title={step.cmd}>{step.cmd}</code>
          <button class="btn ghost sm" onclick={() => copy(`cmd${i}`, step.cmd)}>{copied === `cmd${i}` ? 'Copied ✓' : 'Copy'}</button>
        </div>
      {/each}
      {#if steps.note}<p class="dim note">{steps.note}</p>{/if}
      <div class="actions">
        <button class="btn primary sm" onclick={openCaPage} disabled={!store.proxy.running} title={store.proxy.running ? 'Open the CA download page in your browser' : 'Start the proxy first'}>Open in browser</button>
        <button class="btn sm" onclick={() => copy('pem', ca.pem)}>{copied === 'pem' ? 'Copied ✓' : 'Copy PEM'}</button>
        <button class="btn sm" onclick={exportCa}>Export CA…</button>
        {#if regenConfirm}
          <span class="dim regen-warn">Replaces the CA. You must re-trust the new one.</span>
          <button class="btn sm danger" onclick={regenerateCa} disabled={regenBusy}>{regenBusy ? 'Working…' : 'Confirm'}</button>
          <button class="btn ghost sm" onclick={() => (regenConfirm = false)} disabled={regenBusy}>Cancel</button>
        {:else}
          <button class="btn ghost sm" onclick={() => (regenConfirm = true)}>Regenerate CA…</button>
        {/if}
      </div>
    {/if}
  </section>
  {/if}

  {#if section === 'project'}
  <section class="card">
    <h2>Usage</h2>
    <p class="dim">Live resource usage for this app. Updates every 1.5s while this tab is open; reading it never blocks the proxy.</p>
    <div class="usage-grid">
      <div class="stat">
        <span class="stat-label">Memory (this app)</span>
        <span class="stat-value">{usage ? fmtBytes(usage.memBytes) : '…'}</span>
        <span class="stat-sub">of {usage ? fmtBytes(usage.totalMemBytes) : '…'} total</span>
      </div>
      <div class="stat">
        <span class="stat-label">CPU (this app)</span>
        <span class="stat-value">{usage ? `${usage.cpuPercent.toFixed(1)}%` : '…'}</span>
        <span class="stat-sub">{usage ? `${usage.cpuCount} logical cores` : ''}</span>
      </div>
      <div class="stat">
        <span class="stat-label">Uptime</span>
        <span class="stat-value">{usage ? fmtDuration(usage.uptimeSecs) : '…'}</span>
        <span class="stat-sub">since launch</span>
      </div>
    </div>
    {#if usage}
      <div class="usage-bars">
        <div class="ubar">
          <div class="ubar-head"><span class="dim">Memory</span><span class="mono">{((usage.memBytes / Math.max(1, usage.totalMemBytes)) * 100).toFixed(1)}%</span></div>
          <div class="progress"><div class="fill" style="width: {Math.min(100, (usage.memBytes / Math.max(1, usage.totalMemBytes)) * 100)}%"></div></div>
        </div>
        <div class="ubar">
          <div class="ubar-head"><span class="dim">CPU (of {usage.cpuCount} cores)</span><span class="mono">{(usage.cpuPercent / Math.max(1, usage.cpuCount)).toFixed(1)}%</span></div>
          <div class="progress"><div class="fill" style="width: {Math.min(100, usage.cpuPercent / Math.max(1, usage.cpuCount))}%"></div></div>
        </div>
      </div>
    {/if}
    <h3 class="usage-h3">Crash dumps</h3>
    <p class="dim hint-line">
      Any crash writes a self-contained log (message, location, backtrace, memory) here so you can report it.
    </p>
    {#if crash}
      <div class="kv wide">
        <span class="dim">Folder</span>
        <span class="mono path">{crash.dir ?? 'not available yet'}</span>
        {#if crash.dir}<button class="btn ghost sm" onclick={() => copy('crashdir', crash.dir)}>{copied === 'crashdir' ? 'Copied ✓' : 'Copy path'}</button>{/if}
      </div>
      <p class="dim status-line">
        {crash.count} dump{crash.count === 1 ? '' : 's'}{crash.latest ? ` · latest: ${crash.latest}` : ' · none yet (good news)'}
      </p>
    {/if}
  </section>
  {/if}
  </div>
</div>

<style>
  .settings {
    display: grid;
    grid-template-columns: 216px 1fr;
    height: 100%;
    min-height: 0;
  }
  .sub-nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-height: 0;
    padding: 14px 10px;
    border-right: 1px solid var(--border);
    background: var(--bg-elev);
    overflow: auto;
  }
  .sub-nav-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 12px;
    border: 0;
    border-radius: 8px;
    background: none;
    color: var(--text-dim);
    font: 500 13px var(--sans);
    text-align: left;
    cursor: pointer;
  }
  .sub-nav-item > span:first-child {
    flex: 1;
  }
  .sub-nav-item:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .sub-nav-item.active {
    background: var(--accent-soft);
    color: var(--accent);
    font-weight: 600;
  }
  .sub-nav-item .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .sub-nav-item .dot.on {
    background: var(--green);
  }
  .sub-nav-item .ncount {
    min-width: 18px;
    height: 18px;
    padding: 0 5px;
    border-radius: 99px;
    background: var(--bg-soft);
    color: var(--text-dim);
    font: 600 10.5px var(--sans);
    line-height: 18px;
    text-align: center;
  }
  .sub-nav-item.active .ncount {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    color: var(--accent);
  }
  .content {
    min-height: 0;
    padding: 22px 24px;
    overflow: auto;
  }
  .card {
    max-width: 1100px;
    padding: 20px 24px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
  }
  .card h2 {
    font-size: 16px;
  }
  /* Keep descriptive text readable while controls use the full width. */
  .card > p {
    max-width: 780px;
  }
  h2 {
    margin: 0 0 4px;
    font-size: 14px;
    font-weight: 600;
  }
  p {
    margin: 0 0 14px;
    font-size: 12.5px;
    line-height: 1.5;
  }
  .form {
    display: flex;
    align-items: flex-end;
    gap: 10px;
  }
  .bform {
    display: grid;
    gap: 10px;
    max-width: 640px;
  }
  .bform .input {
    width: 100%;
  }
  .btn-row {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .cdp {
    display: grid;
    gap: 8px;
    margin-top: 6px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }
  .cdp-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    cursor: pointer;
  }
  .cdp-row input {
    accent-color: var(--accent);
  }
  .cdp-port {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12.5px;
  }
  .cdp-port .port {
    width: 110px;
  }
  .cdp .hint {
    margin: 0;
    font-size: 11.5px;
  }
  .prof-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 2px 0 2px 24px;
  }
  .prof-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .prof-row .input {
    flex: 1;
    min-width: 0;
    height: 28px;
  }
  .add-prof {
    align-self: flex-start;
  }
  .progress {
    height: 6px;
    margin: 8px 0 2px;
    border-radius: 99px;
    background: var(--bg-soft);
    overflow: hidden;
  }
  .progress .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s;
  }
  label {
    display: grid;
    gap: 5px;
    font-size: 11.5px;
    color: var(--text-dim);
  }
  label .input {
    width: 150px;
  }
  .select {
    width: 250px !important;
    padding-right: 28px;
    appearance: none;
    background: var(--bg)
      url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='%238a92a6' stroke-width='2.5' stroke-linecap='round'%3E%3Cpath d='m6 9 6 6 6-6'/%3E%3C/svg%3E")
      no-repeat right 9px center / 12px;
    font-family: var(--mono);
    font-size: 12px;
    cursor: pointer;
  }
  .scope {
    width: 100%;
    height: 110px;
    padding: 8px 10px;
    resize: vertical;
    line-height: 1.6;
  }
  .sitemap-head {
    display: flex;
    align-items: baseline;
    gap: 12px;
    margin-top: 16px;
  }
  .sitemap-head .cap {
    font: 600 10.5px var(--sans);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }
  .sitemap-box {
    margin-top: 6px;
    max-height: 260px;
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
  }
  .count {
    align-self: center;
    font-size: 12px;
  }
  .reach {
    margin: 12px 0 0;
  }
  .kv {
    display: grid;
    grid-template-columns: 56px 1fr auto;
    align-items: center;
    gap: 10px;
    padding: 8px 0;
    border-top: 1px solid var(--border);
    font-size: 12.5px;
  }
  .kv.wide {
    grid-template-columns: 110px 1fr auto;
  }
  .platforms {
    display: flex;
    gap: 2px;
    width: fit-content;
    margin: 6px 0 4px;
    padding: 2px;
    border-radius: 7px;
    background: var(--bg-soft);
  }
  .plat {
    height: 24px;
    padding: 0 10px;
    border: 0;
    border-radius: 5px;
    background: none;
    color: var(--text-dim);
    font: 500 12px var(--sans);
    cursor: pointer;
  }
  .plat.active {
    background: var(--bg-elev);
    color: var(--text);
    box-shadow: 0 1px 2px rgb(0 0 0 / 0.25);
  }
  .path {
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .actions {
    display: flex;
    gap: 8px;
    padding-top: 10px;
  }
  .actions.oos {
    align-items: center;
    flex-wrap: wrap;
    margin-top: 10px;
    border-top: 1px solid var(--border);
  }
  .icon-btn {
    padding: 2px 5px;
    display: inline-flex;
    align-items: center;
    vertical-align: middle;
    line-height: 0;
  }
  .note {
    margin: 12px 0 0;
  }
  .error {
    margin: 10px 0 0;
    color: var(--red);
  }
  .btn.danger {
    border-color: color-mix(in srgb, var(--red) 55%, transparent);
    color: var(--red);
  }
  .regen-warn {
    align-self: center;
    font-size: 12px;
  }
  .error.inline {
    align-self: center;
    margin: 0;
    font-size: 12px;
  }
  .rules {
    display: grid;
    gap: 6px;
  }
  .rule {
    display: grid;
    grid-template-columns: 16px 140px 1fr 1fr 110px 26px;
    gap: 6px;
    align-items: center;
  }
  .rule.off .input {
    opacity: 0.5;
  }
  .rule input[type='checkbox'] {
    accent-color: var(--accent);
  }
  .rule .input {
    min-width: 0;
    height: 28px;
    padding: 0 8px;
  }
  .compact {
    font-size: 12px;
    cursor: pointer;
  }
  .grow {
    flex: 1;
  }
  .tester {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid var(--border);
  }
  .tester-controls {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 8px;
  }
  .radio {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 12.5px;
    cursor: pointer;
  }
  .radio input {
    accent-color: var(--accent);
  }
  .tester-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .tester-pane {
    display: grid;
    gap: 4px;
    font-size: 11.5px;
  }
  .tester-pane textarea {
    height: 150px;
    padding: 8px 10px;
    resize: vertical;
    line-height: 1.5;
  }
  .grow .input {
    width: 100%;
  }
  .status-line {
    margin: 10px 0 0;
    font-size: 12px;
  }
  .hint-line {
    margin: 6px 0 0;
    font-size: 12px;
    line-height: 1.5;
  }
  .invisible {
    margin-top: 12px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 10px;
    color: var(--text);
    font-size: 12.5px;
    cursor: pointer;
  }
  .check input {
    accent-color: var(--accent);
  }
  .warn {
    margin-top: 12px;
    padding: 9px 12px;
    border: 1px solid color-mix(in srgb, var(--amber) 45%, transparent);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--amber) 10%, transparent);
    color: var(--text);
    font-size: 12.5px;
    line-height: 1.5;
  }
  .warn.danger {
    border-color: color-mix(in srgb, var(--red) 50%, transparent);
    background: color-mix(in srgb, var(--red) 10%, transparent);
  }
  /* Usage panel */
  .usage-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 10px;
    margin: 6px 0 14px;
  }
  .stat {
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-soft);
  }
  .stat-label {
    color: var(--text-dim);
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .stat-value {
    font: 700 22px var(--mono);
    color: var(--text);
  }
  .stat-sub {
    color: var(--text-faint);
    font-size: 11.5px;
  }
  .usage-bars {
    display: flex;
    flex-direction: column;
    gap: 10px;
    margin-bottom: 8px;
  }
  .ubar-head {
    display: flex;
    justify-content: space-between;
    font-size: 11.5px;
    margin-bottom: 4px;
  }
  .usage-h3 {
    margin: 16px 0 4px;
    font-size: 13px;
  }
</style>
