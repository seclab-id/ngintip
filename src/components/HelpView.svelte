<script>
  // In-app guide so features are discoverable without leaving the app.
  import { store } from '../lib/store.svelte.js';
  import { BRAND } from '../lib/brand.js';

  const MARK = '§';
  const go = (view) => (store.view = view);

  // { title, body } sections. `body` is an array of strings or [term, desc] pairs.
  const sections = [
    {
      title: 'Getting started',
      items: [
        `On launch, pick a project (Continue, New, Open, or Blank). A project is one .${BRAND.ext} SQLite file holding this session's history, scope, rules and Repeater tabs.`,
        'The proxy listens on 127.0.0.1:8080. Point your browser or device at it.',
        'To intercept HTTPS, install and trust the CA: Settings > CA certificate has copy-paste steps for macOS, Windows, Linux, Firefox, Burp Suite and phones.',
      ],
    },
    {
      title: 'HTTP History',
      items: [
        ['Search', 'The top box searches everything at once: host, path, method, status, MIME and notes, plus request/response bodies and WebSocket messages (full-text index). Body results are unioned in automatically as you type; press Enter to run the body search immediately.'],
        ['Viewer search', 'Inside the request or response viewer, click Search or press Cmd/Ctrl+F to find text in that message. Enter / Shift+Enter jump between matches.'],
        ['Show all', 'Large bodies are shown up to 512 KB; click Show all to load the full body (bodies are stored in full).'],
        ['Resizable panes', 'Drag the divider between the request and response panes; double-click it to reset.'],
        ['Sitemap', 'The Sitemap button opens a host > path tree; click a node to filter the table, or right-click it to add its host (or *.host) to the scope.'],
        ['Right-click a row', 'Send to Repeater, Send to Intruder, Copy URL, Copy as cURL, Capture image, highlight, or delete.'],
        ['Export', 'Export HAR or Burp Suite items XML from the toolbar.'],
      ],
    },
    {
      title: 'Intercept',
      items: [
        'Turn Intercept on (topbar or the Intercept view) to hold in-scope requests before they are sent.',
        ['Forward / Drop', 'Forward (Cmd+Enter) sends the held message; Drop (Shift+Cmd+Backspace) blocks it.'],
        ['Responses', 'Enable "Intercept responses" to hold every in-scope response, or use "Forward + response" (Shift+Cmd+Enter) to hold just the response to the current request.'],
        ['Original vs Edited', 'The editor has an Edited tab (what will be sent) and an Original tab (what arrived) so you can compare your changes.'],
      ],
    },
    {
      title: 'Match & Replace',
      items: [
        'Settings > Match & Replace: regex rules on the request line, request/response headers, and bodies, applied automatically before Intercept.',
        ['Headers', 'An empty match adds a header; a rule that produces an empty line removes it.'],
        ['Test rules', 'Click Test rules, paste a sample request or response, and see the result without touching live traffic.'],
      ],
    },
    {
      title: 'Repeater',
      items: [
        'Send a request repeatedly and tweak it. Send with the button or Cmd+Enter.',
        ['History', 'The < and > buttons step through requests you already sent from this tab (with their responses and dates). Unsent edits are kept and reachable at the end.'],
        ['Grouping', 'Right-click a tab > New group / Move to <group>, or drag tabs in and out of groups. Right-click a group label to rename, colour or delete it; click it to collapse. Groups and colours are saved per project.'],
        ['Protocol', 'The version on the request line picks the protocol: write HTTP/2 to send over HTTP/2, otherwise HTTP/1.1.'],
        ['Edit & resend', 'The request is a plain, fully editable text area — change the method, path, any header or the body directly. After sending you stay in edit mode; keep changing and Send again. + Header / + Body add lines, and Content-Length recomputes to match the body (it is also fixed automatically on send).'],
        ['Capture', 'Turn the request/response into an image (it reflects how far you scrolled each pane), annotate with box, ellipse, arrow, line, pen, highlighter, text, redact and numbered steps in any colour/width, then Save PNG or Copy image. Undo Cmd/Ctrl+Z, redo Cmd/Ctrl+Y (or Shift+Cmd+Z). Also available from the History right-click menu.'],
      ],
    },
    {
      title: 'Intruder',
      items: [
        `Mark payload positions by wrapping text in section signs (${MARK}...${MARK}). Use the "Add ${MARK}${MARK}" button, then type the payload between the signs.`,
        ['Sniper', 'One payload set; one position at a time (others left at their default).'],
        ['Battering ram', 'One payload set; the same payload in every position.'],
        ['Pitchfork', 'One set per position; sets advance in parallel (shortest wins).'],
        ['Cluster bomb', 'One set per position; every combination.'],
        ['Run', 'Set the thread count and Start attack. The results table sorts by status, length, time or payload; click a row to see its response.'],
      ],
    },
    {
      title: 'Decoder',
      items: [
        'Encode/decode Base64, Base64URL, URL, HTML entities and hex, decode JWTs, and hash with MD5 or SHA-1/256/512.',
        'Click operations to build a recipe (multiple steps, e.g. URL → Base64 → Hex); each step feeds the next and the output updates live. Reorder or remove steps, or select text in an HTTP/S response and "Send to Decoder" to run it through the recipe.',
      ],
    },
    {
      title: 'Proxy & network',
      items: [
        ['Listener', 'Settings > Proxy listener: choose the interface. 0.0.0.0 exposes it to the LAN (for testing from a phone); the topbar warns when it is reachable.'],
        ['Authentication', 'Optionally require a username/password (Proxy-Authorization: Basic).'],
        ['Upstream proxy', 'Forward all traffic through http:// or socks5:// (Tor, a corporate proxy, or another tool).'],
        ['Invisible proxy', 'For clients that ignore proxy settings (thick clients, Flutter). Redirect them here (hosts file / firewall); direct connections are routed by TLS SNI or the Host header. TLS targets are assumed on port 443.'],
        ['Scope', 'Host patterns like example.com or *.example.com. Intercept only holds in-scope requests; History can be filtered by In scope.'],
      ],
    },
    {
      title: 'Logs',
      items: [
        'The Logs view (sidebar) shows proxy activity and errors: TLS handshake failures (often certificate pinning), upstream connection failures, WebSocket errors, and info/debug about start/stop and project changes.',
        ['Filter', 'Filter by level (error/warn/info/debug) or text; Follow sticks to the newest line.'],
        'A badge on the sidebar counts new errors/warnings since you last opened it.',
      ],
    },
    {
      title: 'Other',
      items: [
        ['Theme', 'The topbar button cycles system / light / dark.'],
        ['Save as', 'Settings > Project > Save as writes a clean copy and continues in the new file.'],
        ['Test server', 'Settings > Test server runs a local echo server (127.0.0.1) that reflects what it received, including whether Content-Length matched the body — point Repeater at it (Try in Repeater) to verify edits.'],
        ['WebSocket', 'ws:// and wss:// are bridged; open a 101 flow in History to see its messages.'],
      ],
    },
  ];

  const shortcuts = [
    ['Cmd/Ctrl + F', 'Search inside a request/response viewer'],
    ['Shift + Enter', 'Search bodies (History filter box)'],
    ['Cmd/Ctrl + Enter', 'Send (Repeater) / Forward (Intercept)'],
    ['Shift + Cmd + Enter', 'Forward request and hold its response (Intercept)'],
    ['Shift + Cmd + Backspace', 'Drop the held message (Intercept)'],
  ];
</script>

<div class="help">
  <div class="inner">
    <h1>{BRAND.name} guide</h1>
    <p class="dim lead">A quick tour of where each feature lives. Jump in from the sidebar on the left.</p>

    {#each sections as sec}
      <section class="card">
        <h2>{sec.title}</h2>
        <ul>
          {#each sec.items as item}
            <li>
              {#if Array.isArray(item)}<b>{item[0]}.</b> {item[1]}{:else}{item}{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/each}

    <section class="card">
      <h2>Keyboard shortcuts</h2>
      <table>
        <tbody>
          {#each shortcuts as [keys, what]}
            <tr><td class="mono keys">{keys}</td><td>{what}</td></tr>
          {/each}
        </tbody>
      </table>
    </section>

    <div class="jump">
      <span class="dim">Open:</span>
      <button class="btn sm" onclick={() => go('history')}>History</button>
      <button class="btn sm" onclick={() => go('intercept')}>Intercept</button>
      <button class="btn sm" onclick={() => go('repeater')}>Repeater</button>
      <button class="btn sm" onclick={() => go('intruder')}>Intruder</button>
      <button class="btn sm" onclick={() => go('decoder')}>Decoder</button>
      <button class="btn sm" onclick={() => go('settings')}>Settings</button>
    </div>
  </div>
</div>

<style>
  .help {
    height: 100%;
    overflow: auto;
  }
  .inner {
    max-width: 780px;
    margin: 0 auto;
    padding: 24px 20px 60px;
  }
  h1 {
    margin: 0 0 4px;
    font-size: 22px;
  }
  .lead {
    margin: 0 0 18px;
    font-size: 13px;
  }
  .card {
    margin-bottom: 14px;
    padding: 16px 20px;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--bg-elev);
  }
  h2 {
    margin: 0 0 10px;
    font-size: 14px;
  }
  ul {
    margin: 0;
    padding-left: 18px;
    display: grid;
    gap: 7px;
  }
  li {
    font-size: 13px;
    line-height: 1.5;
  }
  b {
    color: var(--text);
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }
  td {
    padding: 5px 0;
    border-top: 1px solid var(--border);
    vertical-align: top;
  }
  td:first-child {
    padding-top: 6px;
  }
  .keys {
    width: 200px;
    color: var(--accent);
  }
  .jump {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-top: 6px;
  }
</style>
