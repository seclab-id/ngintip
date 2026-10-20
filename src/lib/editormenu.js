// Shared "+ Header" / "+ Body" dropdown items for the raw-request editors, so the
// Repeater and the Intercept editor stay in lockstep: any change to one flow
// changes both. `get` returns the current raw text; `set` replaces it.

import { addHeader, addBody, fixContentLength, hasBody, setHeader, setBody, getHeader } from './httpmsg.js';
import { HEADER_PRESETS, randomUserAgent } from './headers.js';
import { BODY_SKELETONS } from './bodies.js';

/** Items for the "+ Header" menu: blank line, random UA, Content-Length, presets. */
export function headerMenuItems(get, set) {
  const items = [
    { label: 'Blank header', hint: '+', run: () => set(addHeader(get())) },
    {
      label: 'Random User-Agent',
      hint: '⟳',
      run: () => set(setHeader(get(), 'User-Agent', randomUserAgent(getHeader(get(), 'User-Agent')))),
    },
    { label: 'Content-Length (match body)', hint: 'Σ', run: () => set(fixContentLength(get())) },
    'separator',
  ];
  let group = null;
  for (const p of HEADER_PRESETS) {
    if (p.group !== group) {
      group = p.group;
      items.push({ label: group, disabled: true });
    }
    items.push({ label: `${p.name}: ${p.value || '…'}`, run: () => set(setHeader(get(), p.name, p.value)) });
  }
  return items;
}

/** Items for the "+ Body" menu: empty body plus JSON/form/XML/... skeletons. */
export function bodyMenuItems(get, set) {
  const items = [];
  if (!hasBody(get())) {
    items.push({ label: 'Empty body', hint: '+', run: () => set(addBody(get(), '')) });
    items.push('separator');
  }
  for (const s of BODY_SKELETONS) {
    items.push({ label: s.label, run: () => set(setBody(get(), s.body, s.contentType)) });
  }
  return items;
}
