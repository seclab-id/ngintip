// Keep a drag-selection inside the pane it started in.
//
// Request and response are two panes of one document, so a native drag that leaves the
// pane keeps extending through the DOM and lands in its neighbour: drag upwards in the
// response and the tail of the request highlights too, because everything between the
// anchor and the pointer is selected. Copying then yields both messages.
//
// The fix is to make the other panes unselectable for the duration of the drag. A
// `user-select: none` subtree is skipped by the selection rather than clipped out of
// it, so the drag simply cannot reach them, and nothing has to fight the browser by
// rewriting ranges on every `selectionchange`.
//
// It is done by toggling an attribute, not inline styles, because the text that needs
// locking sets `user-select: text` on itself (`.code` in app.css). An inherited `none`
// on the pane root loses to that, so the rule has to reach every descendant and
// outrank it - see the `[data-selectlock]` rule in app.css. Toggling one attribute also
// means the restore cannot get the previous value wrong: nothing else is touched.

/** Panes that take part. Set on each message panel's root element. */
export const PANE_ATTR = 'data-msgpane';

/** Set on the panes that are locked while a drag is in progress elsewhere. */
export const LOCK_ATTR = 'data-selectlock';

/**
 * Make every pane except `root` unselectable. Returns the function that puts them back;
 * call it when the drag ends (pointerup and pointercancel both count: a drag that ends
 * off-window or is interrupted must not leave the other panes dead).
 */
export function isolateSelection(root, doc = typeof document === 'undefined' ? null : document) {
  if (!root || !doc) return () => {};
  const others = Array.from(doc.querySelectorAll(`[${PANE_ATTR}]`)).filter(
    // Skip self, and skip anything nested either way: locking an ancestor would lock
    // the pane being dragged in along with it.
    (n) => n !== root && !n.contains(root) && !root.contains(n),
  );
  if (!others.length) return () => {};

  for (const n of others) n.setAttribute(LOCK_ATTR, '');

  let done = false;
  return () => {
    if (done) return;
    done = true;
    for (const n of others) n.removeAttribute(LOCK_ATTR);
  };
}
