// Presets and helpers for the Repeater "+ Header" menu. Values are geared at
// manual security testing (origin-IP spoofing, host-header tricks, fingerprint).

// A small pool of realistic User-Agent strings for the "Random User-Agent" action.
export const USER_AGENTS = [
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36',
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15',
  'Mozilla/5.0 (X11; Linux x86_64; rv:125.0) Gecko/20100101 Firefox/125.0',
  'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1',
  'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Mobile Safari/537.36',
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:125.0) Gecko/20100101 Firefox/125.0',
  'curl/8.7.1',
  'PostmanRuntime/7.37.0',
  'Googlebot/2.1 (+http://www.google.com/bot.html)',
];

/** A random User-Agent, preferring one different from `current`. */
export function randomUserAgent(current = '') {
  const others = USER_AGENTS.filter((u) => u !== current.trim());
  const pool = others.length ? others : USER_AGENTS;
  return pool[Math.floor(Math.random() * pool.length)];
}

// A focused set of headers useful for manual pentesting, grouped for the menu.
// Selecting one adds the header, or replaces its value if it already exists.
export const HEADER_PRESETS = [
  // Access-control / IP spoofing bypass (point the origin IP at localhost).
  { group: 'Bypass · spoof IP', name: 'X-Forwarded-For', value: '127.0.0.1' },
  { group: 'Bypass · spoof IP', name: 'X-Real-IP', value: '127.0.0.1' },
  { group: 'Bypass · spoof IP', name: 'True-Client-IP', value: '127.0.0.1' },
  { group: 'Bypass · spoof IP', name: 'Forwarded', value: 'for=127.0.0.1;host=localhost;proto=https' },

  // Host-header / URL-rewrite tricks (auth/ACL bypass, cache poisoning).
  { group: 'Bypass · host/URL', name: 'X-Forwarded-Host', value: 'localhost' },
  { group: 'Bypass · host/URL', name: 'X-Original-URL', value: '/admin' },
  { group: 'Bypass · host/URL', name: 'X-Rewrite-URL', value: '/admin' },

  // Auth.
  { group: 'Auth', name: 'Authorization', value: 'Bearer ' },
  { group: 'Auth', name: 'Cookie', value: '' },
  { group: 'Auth', name: 'X-Api-Key', value: '' },

  // CORS / CSRF / content-type probing.
  { group: 'Request shaping', name: 'Origin', value: 'https://evil.example' },
  { group: 'Request shaping', name: 'Referer', value: 'https://www.google.com/' },
  { group: 'Request shaping', name: 'X-Requested-With', value: 'XMLHttpRequest' },
  { group: 'Request shaping', name: 'Content-Type', value: 'application/json' },
];
