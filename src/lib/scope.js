// Mirrors src-tauri/src/scope.rs so History can filter without a round trip.

/** Strip a port and lowercase: what every pattern is compared against. */
function normalizeHost(host) {
  return host.replace(/:\d+$/, '').toLowerCase();
}

/** Whether `pattern` matches a host that is ALREADY normalized. */
function matchesNormalized(pattern, h) {
  if (pattern.startsWith('*.')) {
    const base = pattern.slice(2);
    return h === base || h.endsWith(`.${base}`);
  }
  return h === pattern;
}

/** `*.example.com` matches example.com and subdomains; otherwise exact. Port ignored. */
export function hostMatches(pattern, host) {
  return matchesNormalized(pattern, normalizeHost(host));
}

/** Empty scope = everything in scope.
 *
 *  The host is normalized ONCE here instead of once per pattern. History filters every
 *  row through this whenever the list is rebuilt, and the regex plus `toLowerCase` sat
 *  inside the pattern loop: with three patterns and no match, a 39,662-row project did
 *  119,000 of them per pass. The per-pattern work left is a string compare.
 */
export function inScope(patterns, host) {
  if (!patterns.length) return true;
  const h = normalizeHost(host);
  for (const p of patterns) {
    if (matchesNormalized(p, h)) return true;
  }
  return false;
}
