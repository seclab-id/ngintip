// Host → path tree built from History rows.

/** `scheme://host/path` without the query string. */
export function flowKey(f) {
  return `${f.scheme}://${f.host}${f.path.split('?')[0]}`;
}

/**
 * Tree of `{ key, name, count, children }`, one root per scheme://host,
 * then one level per path segment. Children are sorted by name.
 */
export function buildSitemap(flows) {
  const roots = new Map();
  for (const f of flows) {
    const origin = `${f.scheme}://${f.host}`;
    let node = roots.get(origin);
    if (!node) roots.set(origin, (node = { key: origin, name: origin, count: 0, children: new Map() }));
    node.count++;
    for (const seg of f.path.split('?')[0].split('/').filter(Boolean)) {
      let child = node.children.get(seg);
      if (!child) node.children.set(seg, (child = { key: `${node.key}/${seg}`, name: seg, count: 0, children: new Map() }));
      child.count++;
      node = child;
    }
  }
  const finish = (nodes) =>
    [...nodes.values()]
      .sort((a, b) => a.name.localeCompare(b.name))
      .map((n) => ({ ...n, children: finish(n.children) }));
  return finish(roots);
}

/**
 * Prune the tree to branches that match `query` (case-insensitive, on the node
 * name or its full key). A matching node keeps its whole subtree; a non-matching
 * node is kept only when a descendant matches (so ancestors stay for context).
 * An empty query returns the tree unchanged.
 */
export function filterSitemap(tree, query) {
  const q = (query ?? '').trim().toLowerCase();
  if (!q) return tree;
  const walk = (nodes) => {
    const out = [];
    for (const n of nodes) {
      if (n.name.toLowerCase().includes(q) || n.key.toLowerCase().includes(q)) {
        out.push(n); // matched: keep the whole subtree
      } else {
        const children = walk(n.children);
        if (children.length) out.push({ ...n, children });
      }
    }
    return out;
  };
  return walk(tree);
}

/** Every node key in a tree (used to auto-expand while searching). */
export function allKeys(tree, out = new Set()) {
  for (const n of tree) {
    out.add(n.key);
    allKeys(n.children, out);
  }
  return out;
}

/** Nodes to render, depth-first, descending only into `expanded` keys. */
export function flatten(tree, expanded, depth = 0, out = []) {
  for (const node of tree) {
    out.push({ node, depth });
    if (node.children.length && expanded.has(node.key)) flatten(node.children, expanded, depth + 1, out);
  }
  return out;
}

/** Does the flow sit at or below the node `key`? */
export function underNode(f, key) {
  const k = flowKey(f);
  return k === key || k.startsWith(key.endsWith('/') ? key : `${key}/`);
}
