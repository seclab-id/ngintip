// Build the next request to follow an HTTP redirect (3xx + Location).
// Pure + testable; used by the Repeater/Intercept "Follow redirect" buttons.

import { splitMsg } from './httpmsg.js';

/** { status, location } for a redirect response, or null when it isn't one. */
export function redirectInfo(responseRaw = '') {
  const { head } = splitMsg(responseRaw);
  const lines = head.split('\n');
  const status = parseInt((lines[0] || '').split(/\s+/)[1], 10) || 0;
  if (status < 300 || status >= 400) return null;
  for (const l of lines.slice(1)) {
    const i = l.indexOf(':');
    if (i > 0 && l.slice(0, i).trim().toLowerCase() === 'location') {
      const location = l.slice(i + 1).trim();
      if (location) return { status, location };
    }
  }
  return null;
}

/**
 * Given the request that was sent, its redirect response, and the base target
 * (scheme://host), return the next `{ target, raw }` to send, or null if there's
 * no redirect. 303 and (per browsers) 301/302 become GET with no body; 307/308
 * keep the method and body.
 */
export function followRequest(requestRaw = '', responseRaw = '', currentTarget = '') {
  const info = redirectInfo(responseRaw);
  if (!info) return null;

  const { head, body } = splitMsg(requestRaw);
  const lines = head.split('\n');
  const [method0 = 'GET', path0 = '/', version = 'HTTP/1.1'] = (lines[0] || '').split(/\s+/);

  // Resolve the (possibly relative) Location against the request's absolute URL.
  let base;
  try {
    base = new URL(path0, currentTarget || 'http://localhost');
  } catch {
    base = undefined;
  }
  let dest;
  try {
    dest = new URL(info.location, base);
  } catch {
    return null;
  }

  const keep = info.status === 307 || info.status === 308;
  const method = keep ? method0 : 'GET';
  const newPath = dest.pathname + dest.search;

  const out = [`${method} ${newPath} ${version}`];
  let hasHost = false;
  for (const l of lines.slice(1)) {
    const name = l.slice(0, l.indexOf(':')).trim().toLowerCase();
    if (name === 'host') {
      out.push(`Host: ${dest.host}`);
      hasHost = true;
      continue;
    }
    if (name === 'content-length') continue; // recomputed on send
    if (!keep && (name === 'content-type' || name === 'content-encoding')) continue;
    out.push(l);
  }
  if (!hasHost) out.push(`Host: ${dest.host}`);

  const newBody = keep ? body : '';
  const raw = newBody ? `${out.join('\n')}\n\n${newBody}` : `${out.join('\n')}\n\n`;
  return { target: `${dest.protocol}//${dest.host}`, raw, status: info.status, location: info.location };
}
