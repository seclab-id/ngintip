<script>
  // Rendered note body. The HTML comes from lib/markdown.js, which escapes every
  // character of the source before wrapping any of it in a tag, so `{@html}` here can
  // only ever emit this renderer's own elements. See that file for why that matters.
  import { renderMarkdown } from '../lib/markdown.js';
  import * as api from '../lib/api.js';

  let { source = '' } = $props();

  let html = $derived(renderMarkdown(source));
  let el = $state(null);

  // Links open in the user's browser. A plain anchor would navigate the webview itself,
  // which throws away the whole session. Listeners are attached here rather than with
  // `onclick` on the container so this stays a div with no interactive role.
  $effect(() => {
    const node = el;
    if (!node) return;
    const open = (e) => {
      const a = e.target?.closest?.('a[href]');
      if (!a) return;
      e.preventDefault();
      api.openUrl(a.getAttribute('href')).catch(() => {});
    };
    node.addEventListener('click', open);
    // Middle-click and cmd-click would otherwise slip past the click handler.
    node.addEventListener('auxclick', open);
    return () => {
      node.removeEventListener('click', open);
      node.removeEventListener('auxclick', open);
    };
  });
</script>

<div class="md" bind:this={el}>{@html html}</div>

<style>
  .md {
    overflow: auto;
    padding: 10px 12px;
    font-size: 13px;
    line-height: 1.6;
    color: var(--text, #e6e6e6);
    overflow-wrap: anywhere;
    /* `body` in app.css turns selection off app-wide and each text surface opts back
       in (see `.input` and `.code` there). A note you cannot copy out of is a note you
       cannot use, so this is one of them. A pane locked mid-drag still overrides it:
       `[data-selectlock]` is marked important. */
    user-select: text;
    -webkit-user-select: text;
    cursor: text;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4),
  .md :global(h5),
  .md :global(h6) {
    margin: 1.1em 0 0.45em;
    line-height: 1.3;
    font-weight: 700;
  }
  .md :global(h1:first-child),
  .md :global(h2:first-child),
  .md :global(h3:first-child) {
    margin-top: 0;
  }
  .md :global(h1) { font-size: 18px; }
  .md :global(h2) { font-size: 16px; }
  .md :global(h3) { font-size: 14.5px; }
  .md :global(h4),
  .md :global(h5),
  .md :global(h6) { font-size: 13px; }
  .md :global(h1),
  .md :global(h2) {
    padding-bottom: 0.25em;
    border-bottom: 1px solid var(--border, #2a2e38);
  }
  .md :global(p) {
    margin: 0 0 0.75em;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 0 0 0.75em;
    padding-left: 1.4em;
  }
  .md :global(li) {
    margin: 0.15em 0;
  }
  .md :global(li > ul),
  .md :global(li > ol) {
    margin: 0.15em 0 0;
  }
  .md :global(li.md-task) {
    list-style: none;
    margin-left: -1.2em;
  }
  .md :global(li.md-task input) {
    margin-right: 6px;
    vertical-align: -1px;
    accent-color: var(--accent, #e8722c);
  }
  .md :global(code) {
    padding: 0.12em 0.35em;
    border-radius: 4px;
    background: var(--bg, #12141a);
    border: 1px solid var(--border, #2a2e38);
    font: 12px/1.5 var(--mono, ui-monospace, monospace);
  }
  .md :global(pre.md-pre) {
    margin: 0 0 0.8em;
    padding: 9px 11px;
    overflow: auto;
    border-radius: 6px;
    background: var(--bg, #12141a);
    border: 1px solid var(--border, #2a2e38);
  }
  .md :global(pre.md-pre code) {
    padding: 0;
    border: 0;
    background: none;
    white-space: pre;
  }
  .md :global(pre.md-raw) {
    margin: 0;
    white-space: pre-wrap;
    font: 12px/1.5 var(--mono, ui-monospace, monospace);
  }
  .md :global(blockquote) {
    margin: 0 0 0.8em;
    padding: 0.1em 0 0.1em 0.9em;
    border-left: 3px solid var(--border, #2a2e38);
    color: var(--text-dim, #8b90a0);
  }
  .md :global(blockquote > p:last-child) {
    margin-bottom: 0;
  }
  .md :global(hr) {
    margin: 1.1em 0;
    border: 0;
    border-top: 1px solid var(--border, #2a2e38);
  }
  .md :global(a) {
    color: var(--accent, #e8722c);
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .md :global(table.md-table) {
    margin: 0 0 0.9em;
    border-collapse: collapse;
    font-size: 12.5px;
  }
  .md :global(table.md-table th),
  .md :global(table.md-table td) {
    padding: 5px 9px;
    border: 1px solid var(--border, #2a2e38);
    text-align: left;
  }
  .md :global(table.md-table th) {
    background: var(--bg, #12141a);
    font-weight: 600;
  }
  .md :global(.md-c) { text-align: center; }
  .md :global(.md-r) { text-align: right; }
  .md :global(.md-l) { text-align: left; }
  .md :global(strong) { font-weight: 700; }
  .md :global(del) { color: var(--text-dim, #8b90a0); }
</style>
