// Mirrors src-tauri/src/scope.rs so History can filter without a round trip.

/** `*.example.com` matches example.com and subdomains; otherwise exact. Port ignored. */
export function hostMatches(pattern, host) {
  const h = host.replace(/:\d+$/, '').toLowerCase();
  if (pattern.startsWith('*.')) {
    const base = pattern.slice(2);
    return h === base || h.endsWith(`.${base}`);
  }
  return h === pattern;
}

/** Empty scope = everything in scope. */
export function inScope(patterns, host) {
  return !patterns.length || patterns.some((p) => hostMatches(p, host));
}
