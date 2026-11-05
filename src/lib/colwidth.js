// Resizable table columns: widths in px, remembered per table (per machine) in
// localStorage so a layout survives reopening the app. Used by the Organizer tables;
// the History table's column *catalog* lives in columns.js, which is a different thing.
import { brandKey } from './brand.js';

export const MIN_COL = 32;
export const MAX_COL = 900;

export const clampCol = (w) => Math.max(MIN_COL, Math.min(MAX_COL, Math.round(w)));

/** Remembered widths for `key`, or a copy of `defaults` when nothing usable is stored
 *  (missing, corrupt, a different column count, or out-of-range values). */
export function loadColumnWidths(key, defaults) {
  try {
    const raw = JSON.parse(localStorage.getItem(brandKey(`cols.${key}`)));
    if (Array.isArray(raw) && raw.length === defaults.length && raw.every((n) => Number.isFinite(n) && n >= MIN_COL && n <= MAX_COL)) {
      return raw.map((n) => Math.round(n));
    }
  } catch {
    // storage unavailable or corrupt; fall back to the defaults
  }
  return [...defaults];
}

export function saveColumnWidths(key, widths) {
  try {
    localStorage.setItem(brandKey(`cols.${key}`), JSON.stringify([...widths]));
  } catch {
    // not critical: the widths just won't be remembered
  }
}

/** `grid-template-columns` for these widths, plus a flexible filler track so a row
 *  still spans the full table when the columns together are narrower than it. */
export const columnTemplate = (widths) => `${widths.map((w) => `${w}px`).join(' ')} 1fr`;
