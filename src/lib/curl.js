// "Copy as cURL" for a raw request as shown in the viewers.
import { splitMessage } from './message.js';

/** POSIX shell single-quoting. */
export function shellQuote(s) {
  return `'${s.replace(/'/g, `'\\''`)}'`;
}

const PLACEHOLDER = /^\[binary body: \d+ bytes\]$/;

/**
 * curl command for `raw` ("METHOD target VERSION\nheaders\n\nbody").
 * `origin` (e.g. https://example.com) completes origin-form targets.
 */
export function toCurl(raw, origin) {
  const { head, body } = splitMessage(raw.replace(/\r\n/g, '\n'));
  const [start, ...lines] = head.split('\n');
  const [method = 'GET', target = '/'] = start.trim().split(/\s+/);
  const url = /^https?:\/\//i.test(target) ? target : `${origin.replace(/\/$/, '')}${target}`;

  const parts = ['curl', '-sk', '-i'];
  const headers = lines.filter((l) => l.includes(':'));
  // curl sets these itself (and would get Content-Length wrong after edits).
  const skip = /^(content-length|connection|proxy-connection):/i;
  // Viewer notes (e.g. "[Intip: body … truncated]") aren't part of the request.
  const data = body.replace(/\n\n\[Intip: [^\n]*\]$/, '');
  const hasBody = data.length > 0 && !PLACEHOLDER.test(data.trim());

  if (method !== (hasBody ? 'POST' : 'GET')) parts.push('-X', method);
  if (headers.some((h) => /^accept-encoding:/i.test(h))) parts.push('--compressed');
  for (const h of headers) {
    if (!skip.test(h)) parts.push('-H', shellQuote(h.replace(/^([^:]+):\s*/, '$1: ')));
  }
  if (hasBody) parts.push('--data-binary', shellQuote(data));
  parts.push(shellQuote(url));
  return parts.join(' ');
}
