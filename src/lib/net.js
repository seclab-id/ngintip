// Network helpers shared by Settings and the Topbar.

/** Only reachable from this machine (127.0.0.0/8, ::1, localhost). */
export function isLoopback(host = '') {
  const h = host.trim().toLowerCase().replace(/^\[|\]$/g, '');
  return h === 'localhost' || h === '::1' || /^127\./.test(h);
}
