import { describe, expect, it } from 'vitest';
import {
  avatarColor,
  basicAuthHeader,
  basicFor,
  bearerFor,
  cloneFields,
  hostOf,
  kindInfo,
  newField,
  normalizeEntry,
  normalizeUrl,
  templateFields,
  vaultName,
  vaultSubtitle,
  vaultView,
} from './vault.js';

const entry = (fields, extra = {}) => ({ id: 'v', label: '', role: '', note: '', fields, createdAt: 1, updatedAt: 1, ...extra });

describe('vault', () => {
  it('normalizes URLs and extracts the host', () => {
    expect(normalizeUrl('staging.example.com/login')).toBe('https://staging.example.com/login');
    expect(normalizeUrl('http://10.0.0.5:8080')).toBe('http://10.0.0.5:8080');
    expect(normalizeUrl('  ')).toBe('');
    expect(hostOf('https://app.example.com:8443/x?y=1')).toBe('app.example.com:8443');
    expect(hostOf('')).toBe('');
  });

  it('makes fields from kinds and templates (secret by kind)', () => {
    expect(newField('apikey')).toMatchObject({ kind: 'apikey', name: 'API key', secret: true, value: '' });
    expect(newField('email', 'a@b.c')).toMatchObject({ kind: 'email', secret: false, value: 'a@b.c' });
    expect(kindInfo('nope').kind).toBe('text'); // unknown kinds are custom text
    expect(templateFields('email').map((f) => f.kind)).toEqual(['email', 'password', 'url']);
    expect(templateFields('apikey').map((f) => f.kind)).toEqual(['apikey', 'url']);
    expect(templateFields('blank')).toEqual([]);
    const a = templateFields('login');
    const b = cloneFields(a);
    expect(b.map((f) => f.kind)).toEqual(a.map((f) => f.kind));
    expect(b.every((f, i) => f.id !== a[i].id)).toBe(true);
  });

  it('migrates the first flat shape into fields', () => {
    const { entry: e, migrated } = normalizeEntry({ id: 'old', label: 'Admin', username: 'root', password: 'toor', url: '' });
    expect(migrated).toBe(true);
    expect(e.fields.map((f) => [f.kind, f.value, f.secret])).toEqual([
      ['username', 'root', false],
      ['password', 'toor', true],
    ]);
    // An empty old entry gets the usual login fields to fill in.
    expect(normalizeEntry({ id: 'x' }).entry.fields.map((f) => f.kind)).toEqual(['username', 'password', 'url']);
    // The current shape passes through, keeping custom names and secrecy.
    const cur = normalizeEntry({ id: 'n', fields: [{ id: 'f1', kind: 'text', name: 'PIN', secret: true, value: '1234' }] });
    expect(cur.migrated).toBe(false);
    expect(cur.entry.fields[0]).toEqual({ id: 'f1', kind: 'text', name: 'PIN', secret: true, value: '1234' });
  });

  it('names an entry by label, identity, host or a custom value, never a secret', () => {
    expect(vaultName(entry([newField('username', 'root')], { label: 'Admin' }))).toBe('Admin');
    expect(vaultName(entry([newField('email', 'a@x.io'), newField('password', 'p')]))).toBe('a@x.io');
    expect(vaultName(entry([newField('apikey', 'sk_1'), newField('url', 'https://api.x.io/v1')]))).toBe('api.x.io');
    expect(vaultName(entry([newField('apikey', 'sk_1')]))).toBe('Untitled credential');
    // Subtitles show what the name doesn't, then which secrets are held.
    expect(vaultSubtitle(entry([newField('email', 'a@x.io'), newField('url', 'x.io')], { label: 'Admin' }))).toBe('a@x.io · x.io');
    expect(vaultSubtitle(entry([newField('apikey', 'sk_1'), newField('url', 'https://api.x.io')]))).toBe('API key');
    expect(vaultSubtitle(entry([newField('apikey', '')]))).toBe('Empty');
  });

  it('orders by name with new unnamed entries first, and never searches secret values', () => {
    const entries = [
      entry([newField('password', 'hunter2')], { id: 1, label: 'zeta', createdAt: 1 }),
      entry([newField('email', 'a@x.io')], { id: 2, label: 'Alpha', role: 'admin', note: 'from leak', createdAt: 2 }),
      entry([], { id: 3, createdAt: 3 }),
      entry([newField('apikey', 'sk_live_9')], { id: 4, label: 'Payments', createdAt: 4 }),
    ];
    expect(vaultView(entries).map((e) => e.id)).toEqual([3, 2, 4, 1]);
    expect(vaultView(entries, 'ADMIN').map((e) => e.id)).toEqual([2]); // role
    expect(vaultView(entries, 'leak').map((e) => e.id)).toEqual([2]); // note
    expect(vaultView(entries, 'a@x').map((e) => e.id)).toEqual([2]); // a non-secret value
    expect(vaultView(entries, 'api key').map((e) => e.id)).toEqual([4]); // a field name
    expect(vaultView(entries, 'hunter2')).toEqual([]); // secret values never match
    expect(vaultView(entries, 'sk_live')).toEqual([]);
  });

  it('builds UTF-8 safe Basic and Bearer headers from the right fields', () => {
    expect(basicAuthHeader('admin', 's3cret')).toBe('Authorization: Basic YWRtaW46czNjcmV0');
    expect(basicAuthHeader('josé', 'pässwörd')).toBe(`Authorization: Basic ${Buffer.from('josé:pässwörd', 'utf8').toString('base64')}`);
    expect(basicFor(entry([newField('email', 'admin'), newField('password', 's3cret')]))).toBe('Authorization: Basic YWRtaW46czNjcmV0');
    expect(basicFor(entry([newField('apikey', 'sk_1')]))).toBe(''); // nothing to build one from
    expect(bearerFor(entry([newField('token', ' eyJ.abc ')]))).toBe('Authorization: Bearer eyJ.abc');
    expect(bearerFor(entry([newField('password', 'x')]))).toBe('');
  });

  it('gives each entry a stable avatar colour', () => {
    expect(avatarColor('vault-1')).toBe(avatarColor('vault-1'));
    expect(['red', 'orange', 'yellow', 'green', 'cyan', 'blue', 'purple', 'pink']).toContain(avatarColor('anything'));
  });
});
