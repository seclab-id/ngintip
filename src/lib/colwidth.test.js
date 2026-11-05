// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from 'vitest';
import { loadColumnWidths, saveColumnWidths, columnTemplate, clampCol, MIN_COL, MAX_COL } from './colwidth.js';
import { brandKey } from './brand.js';

const KEY = brandKey('cols.test');
const DEFAULTS = [40, 80, 120];

describe('resizable column widths', () => {
  beforeEach(() => localStorage.clear());

  it('falls back to a copy of the defaults when nothing is stored', () => {
    const w = loadColumnWidths('test', DEFAULTS);
    expect(w).toEqual(DEFAULTS);
    w[0] = 999; // a copy: mutating the result must not change the defaults
    expect(DEFAULTS[0]).toBe(40);
  });

  it('round-trips widths through storage', () => {
    saveColumnWidths('test', [50, 90, 130]);
    expect(loadColumnWidths('test', DEFAULTS)).toEqual([50, 90, 130]);
  });

  it('ignores stored widths that are corrupt, the wrong length or out of range', () => {
    localStorage.setItem(KEY, 'not json');
    expect(loadColumnWidths('test', DEFAULTS)).toEqual(DEFAULTS);
    localStorage.setItem(KEY, JSON.stringify([10, 20])); // wrong column count
    expect(loadColumnWidths('test', DEFAULTS)).toEqual(DEFAULTS);
    localStorage.setItem(KEY, JSON.stringify([5, 80, 120])); // below MIN_COL
    expect(loadColumnWidths('test', DEFAULTS)).toEqual(DEFAULTS);
    localStorage.setItem(KEY, JSON.stringify([40, 80, MAX_COL + 1])); // above MAX_COL
    expect(loadColumnWidths('test', DEFAULTS)).toEqual(DEFAULTS);
  });

  it('clamps a dragged width into range and rounds it', () => {
    expect(clampCol(0)).toBe(MIN_COL);
    expect(clampCol(MAX_COL + 500)).toBe(MAX_COL);
    expect(clampCol(120.6)).toBe(121);
  });

  it('builds a grid template with a flexible filler track', () => {
    expect(columnTemplate([40, 80])).toBe('40px 80px 1fr');
  });
});
