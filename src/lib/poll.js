// Run `fn` right away and then every `ms`, but ONLY while the window is visible.
// When the tab/window is hidden the timer is cleared so the webview can fully idle
// and we skip IPC round-trips the user cannot see; it resumes on the next
// visibilitychange, firing once immediately to refresh. Returns a cleanup function
// for use as a Svelte $effect teardown.
export function pollVisible(fn, ms) {
  let timer = null;
  const stop = () => {
    if (timer != null) {
      clearInterval(timer);
      timer = null;
    }
  };
  const start = () => {
    if (timer == null && !document.hidden) {
      fn();
      timer = setInterval(fn, ms);
    }
  };
  const onVis = () => (document.hidden ? stop() : start());
  document.addEventListener('visibilitychange', onVis);
  start();
  return () => {
    stop();
    document.removeEventListener('visibilitychange', onVis);
  };
}
