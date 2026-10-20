// Hide-by-type filter for HTTP History: drop noisy responses (images, fonts,
// CSS, JS, media…) so the table shows the interesting traffic. Pure + testable.

export const HIDE_CATEGORIES = [
  { key: 'images', label: 'Images', exts: ['png', 'jpg', 'jpeg', 'gif', 'svg', 'webp', 'ico', 'bmp', 'avif'], mimes: ['image/'] },
  { key: 'css', label: 'CSS', exts: ['css'], mimes: ['text/css'] },
  { key: 'js', label: 'JavaScript', exts: ['js', 'mjs', 'cjs'], mimes: ['javascript', 'ecmascript'] },
  { key: 'fonts', label: 'Fonts', exts: ['woff', 'woff2', 'ttf', 'otf', 'eot'], mimes: ['font/', 'font-woff', 'x-font'] },
  { key: 'media', label: 'Media (audio/video)', exts: ['mp4', 'webm', 'mp3', 'wav', 'ogg', 'avi', 'mov', 'm4a', 'flac'], mimes: ['video/', 'audio/'] },
  { key: 'docs', label: 'Documents/archives', exts: ['pdf', 'zip', 'gz', 'tar', 'rar', '7z', 'doc', 'docx', 'xls', 'xlsx'], mimes: ['application/pdf', 'application/zip', 'application/gzip', 'application/octet-stream'] },
];

const BY_KEY = Object.fromEntries(HIDE_CATEGORIES.map((c) => [c.key, c]));

/** The lower-case file extension of a request path, or '' when there is none. */
export function extOf(path = '') {
  const clean = path.split(/[?#]/)[0];
  const last = clean.slice(clean.lastIndexOf('/') + 1);
  const dot = last.lastIndexOf('.');
  return dot > 0 ? last.slice(dot + 1).toLowerCase() : '';
}

// `exclude: true` (default) hides the selected types; `exclude: false` shows
// ONLY the selected types (hides everything else).
export function emptyHide() {
  return { cats: [], ext: '', exclude: true };
}

/** Parse the custom extensions text into a clean list. */
export function customExts(ext = '') {
  return ext
    .toLowerCase()
    .split(/[\s,]+/)
    .map((e) => e.replace(/^[.*]+/, '').trim())
    .filter(Boolean);
}

export function hideActive(hide) {
  return !!hide && (hide.cats.length > 0 || customExts(hide.ext).length > 0);
}

/** Whether a flow's type matches any selected category or custom extension. */
export function matchesType(f, hide) {
  const mime = (f.mime || '').toLowerCase();
  const ext = extOf(f.path);
  for (const key of hide.cats) {
    const cat = BY_KEY[key];
    if (!cat) continue;
    if (ext && cat.exts.includes(ext)) return true;
    if (mime && cat.mimes.some((m) => mime.includes(m))) return true;
  }
  const custom = customExts(hide.ext);
  return !!(ext && custom.includes(ext));
}

/**
 * True when a flow should be hidden. In exclude mode (default) the matching
 * types are hidden; in include mode the non-matching types are hidden (so only
 * the selected types show).
 */
export function isHidden(f, hide) {
  if (!hideActive(hide)) return false;
  const matches = matchesType(f, hide);
  // exclude === false means "show only these", so hide anything that doesn't match.
  return hide.exclude === false ? !matches : matches;
}
