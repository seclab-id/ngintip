// The documentation sidebar lives in one list (scripts/docs-nav.mjs) and is written into
// every page. These tests are what make "one list" true rather than aspirational: they
// fail if a page was hand edited, if a page was added to the site but never linked, or if
// a nav entry points at a file that does not exist.

import { describe, it, expect } from 'vitest';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { DOCS, NAV, pages, render, renderPager } from '../../scripts/docs-nav.mjs';

const flat = pages();

describe('docs nav', () => {
  it('is what every page actually contains', () => {
    // The same comparison `node scripts/docs-nav.mjs --check` makes, named per page so a
    // failure says which file drifted instead of just that something did.
    const drifted = [];
    for (const [href] of flat) {
      const source = readFileSync(join(DOCS, href), 'utf8');
      if (render(href, source) !== source) drifted.push(href);
    }
    expect(drifted, 'run: npm run docs:nav').toEqual([]);
  });

  it('points only at pages that exist', () => {
    const missing = flat.filter(([href]) => !existsSync(join(DOCS, href)));
    expect(missing.map(([href]) => href)).toEqual([]);
  });

  it('covers every page on the site', () => {
    // index.html is the landing page: it has no sidebar and is reached by the logo.
    const onDisk = readdirSync(DOCS)
      .filter((f) => f.endsWith('.html') && f !== 'index.html')
      .sort();
    const linked = flat.map(([href]) => href).sort();
    expect(linked).toEqual(onDisk);
  });

  it('lists each page once, under one section', () => {
    const hrefs = flat.map(([href]) => href);
    expect(new Set(hrefs).size).toBe(hrefs.length);
    const labels = flat.map(([, label]) => label);
    expect(new Set(labels).size).toBe(labels.length);
    expect(NAV.every((s) => s.title && s.pages.length)).toBe(true);
  });

  it('marks the open page, and only the open page', () => {
    for (const [href] of flat) {
      const source = readFileSync(join(DOCS, href), 'utf8');
      const toc = source.slice(source.indexOf('<aside class="toc">'), source.indexOf('</aside>'));
      const marked = [...toc.matchAll(/<a href="([^"]+)" aria-current="page">/g)].map((m) => m[1]);
      expect(marked, href).toEqual([href]);
    }
  });

  it('walks the reader forwards and backwards without a dead end', () => {
    const first = flat[0][0];
    const last = flat[flat.length - 1][0];
    expect(renderPager(first)).not.toContain('class="prev"');
    expect(renderPager(last)).not.toContain('class="next"');
    for (const [href] of flat.slice(1, -1)) {
      expect(renderPager(href), href).toContain('class="prev"');
      expect(renderPager(href), href).toContain('class="next"');
    }
  });
});
