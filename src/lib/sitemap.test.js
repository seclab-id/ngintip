import { describe, expect, it } from 'vitest';
import { buildSitemap, filterSitemap, allKeys, flatten, flowKey, underNode } from './sitemap.js';

const flows = [
  { scheme: 'https', host: 'a.com', path: '/api/v1/users?id=1' },
  { scheme: 'https', host: 'a.com', path: '/api/v1/items' },
  { scheme: 'https', host: 'a.com', path: '/' },
  { scheme: 'http', host: 'b.com:8080', path: '/x' },
];

describe('sitemap', () => {
  it('groups by origin then path segment, with counts', () => {
    const tree = buildSitemap(flows);
    expect(tree.map((n) => [n.name, n.count])).toEqual([
      ['http://b.com:8080', 1],
      ['https://a.com', 3],
    ]);
    const rows = flatten(tree, new Set(['https://a.com', 'https://a.com/api', 'https://a.com/api/v1']));
    expect(rows.map(({ node, depth }) => `${depth}:${node.name}:${node.count}`)).toEqual([
      '0:http://b.com:8080:1',
      '0:https://a.com:3',
      '1:api:2',
      '2:v1:2',
      '3:items:1',
      '3:users:1',
    ]);
  });

  it('filters by node without matching sibling prefixes', () => {
    expect(flowKey(flows[0])).toBe('https://a.com/api/v1/users');
    expect(flows.filter((f) => underNode(f, 'https://a.com/api')).length).toBe(2);
    expect(flows.filter((f) => underNode(f, 'https://a.com')).length).toBe(3);
    expect(underNode({ scheme: 'https', host: 'a.com', path: '/apix' }, 'https://a.com/api')).toBe(false);
  });

  it('search filter keeps matching branches (with ancestors) and expands them', () => {
    const tree = buildSitemap(flows);
    // Empty query is a no-op.
    expect(filterSitemap(tree, '')).toBe(tree);

    // Matching a leaf keeps its ancestors and drops non-matching branches.
    const users = filterSitemap(tree, 'users');
    const rows = flatten(users, allKeys(users)).map(({ node, depth }) => `${depth}:${node.name}`);
    expect(rows).toEqual(['0:https://a.com', '1:api', '2:v1', '3:users']);

    // Matching a host keeps that host's whole subtree, and only that host.
    const bOnly = filterSitemap(tree, 'b.com');
    expect(bOnly.map((n) => n.name)).toEqual(['http://b.com:8080']);
    expect(flatten(bOnly, allKeys(bOnly)).map(({ node }) => node.name)).toEqual(['http://b.com:8080', 'x']);

    // Case-insensitive, and no matches yields an empty tree.
    expect(filterSitemap(tree, 'ITEMS').length).toBe(1);
    expect(filterSitemap(tree, 'nope-nothing').length).toBe(0);
  });
});
