// Body skeletons for the Repeater "+ Body" menu. Each sets the body plus a
// matching Content-Type and Content-Length (see setBody in httpmsg.js).
import { BRAND } from './brand.js';

const B = BRAND.multipartBoundary;

export const BODY_SKELETONS = [
  { label: 'JSON', contentType: 'application/json', body: '{\n  "key": "value"\n}' },
  { label: 'Form (urlencoded)', contentType: 'application/x-www-form-urlencoded', body: 'key=value&foo=bar' },
  { label: 'GraphQL', contentType: 'application/json', body: '{\n  "query": "query { __typename }",\n  "variables": {}\n}' },
  { label: 'XML', contentType: 'application/xml', body: '<?xml version="1.0" encoding="UTF-8"?>\n<root>\n  <key>value</key>\n</root>' },
  { label: 'Multipart form', contentType: `multipart/form-data; boundary=${B}`, body: `--${B}\nContent-Disposition: form-data; name="field"\n\nvalue\n--${B}--\n` },
  { label: 'Plain text', contentType: 'text/plain', body: 'body' },
];
