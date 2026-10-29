// File names for new projects: what the user types becomes a safe base name
// ("project pentest x" -> "project_pentest_x"), or a random dated one
// ("29-10-2026-k3f9q2"). The .ngintip extension is added by projectFileName.
import { BRAND } from './brand.js';

// Names Windows refuses as a file base, whatever the extension.
const RESERVED = /^(con|prn|aux|nul|com\d|lpt\d)$/i;
const MAX = 80;

/** Turn typed text into a file base name: spaces -> "_", unsafe characters dropped. */
export function normalizeProjectName(input) {
  let s = String(input ?? '').normalize('NFC').trim();
  // Typed the extension too ("demo.ngintip"): keep only the name.
  s = s.replace(new RegExp(`\\.${BRAND.ext}$`, 'i'), '');
  s = s
    .replace(/\s+/g, '_')
    // Letters/digits of any script stay; path separators, quotes, ":" etc. do not.
    .replace(/[^\p{L}\p{N}._-]+/gu, '_')
    .replace(/_{2,}/g, '_')
    // No leading dot (hidden file) and no dangling separators.
    .replace(/^[._-]+|[._-]+$/g, '');
  s = s.slice(0, MAX).replace(/[._-]+$/, '');
  return RESERVED.test(s) ? `${s}_` : s;
}

const ALPHABET = 'abcdefghijklmnopqrstuvwxyz0123456789';

/** A random dated name: day-month-year-xxxxxx, e.g. 29-10-2026-k3f9q2. */
export function randomProjectName(date = new Date()) {
  const pad = (n) => String(n).padStart(2, '0');
  const bytes = crypto.getRandomValues(new Uint8Array(6));
  const tail = [...bytes].map((b) => ALPHABET[b % ALPHABET.length]).join('');
  return `${pad(date.getDate())}-${pad(date.getMonth() + 1)}-${date.getFullYear()}-${tail}`;
}

/** The file name a typed project name is saved as ('' when nothing usable was typed). */
export function projectFileName(input) {
  const base = normalizeProjectName(input);
  return base ? `${base}.${BRAND.ext}` : '';
}

/** Join a folder and a file name with the folder's own separator (Windows or POSIX). */
export function joinPath(dir, file) {
  const sep = dir.includes('\\') && !dir.includes('/') ? '\\' : '/';
  return dir.endsWith(sep) ? dir + file : dir + sep + file;
}

/** The folder part of a path ('' if it has none). */
export function dirOf(path) {
  const i = Math.max(String(path).lastIndexOf('/'), String(path).lastIndexOf('\\'));
  return i > 0 ? path.slice(0, i) : '';
}
