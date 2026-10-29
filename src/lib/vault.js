// Vault helpers (pure, unit-tested): field kinds and templates, migration of the first
// flat shape, display name / subtitle, list search + order, URL handling, quick-copy
// headers and a stable avatar colour.
//
// An entry is { id, label, role, note, fields: [{ id, kind, name, value, secret }],
// createdAt, updatedAt }: any mix of fields, so a credential can be just an API key, an
// email + password, a token, or anything custom.

/** The kinds of field a credential can hold. Secret ones are masked in the UI, hidden in
 *  MCP listings, and never matched by search. */
export const FIELD_KINDS = [
  { kind: 'username', name: 'Username', secret: false, placeholder: 'admin' },
  { kind: 'email', name: 'Email', secret: false, placeholder: 'admin@example.com' },
  { kind: 'password', name: 'Password', secret: true, placeholder: 'password' },
  { kind: 'apikey', name: 'API key', secret: true, placeholder: 'sk_live_…' },
  { kind: 'token', name: 'Token', secret: true, placeholder: 'eyJhbGciOi… (JWT, bearer or session token)' },
  { kind: 'url', name: 'URL', secret: false, placeholder: 'https://staging.example.com/login' },
  { kind: 'text', name: 'Custom', secret: false, placeholder: 'value' },
];

/** Kind metadata; an unknown kind is treated as custom text. */
export const kindInfo = (kind) => FIELD_KINDS.find((k) => k.kind === kind) ?? FIELD_KINDS[FIELD_KINDS.length - 1];

/** Starting fields for a new credential. */
export const VAULT_TEMPLATES = [
  { id: 'login', label: 'Login', hint: 'username + password', kinds: ['username', 'password', 'url'] },
  { id: 'email', label: 'Email login', hint: 'email + password', kinds: ['email', 'password', 'url'] },
  { id: 'apikey', label: 'API key', hint: 'just the key', kinds: ['apikey', 'url'] },
  { id: 'token', label: 'Token', hint: 'bearer / JWT / session', kinds: ['token', 'url'] },
  { id: 'blank', label: 'Blank', hint: 'add fields yourself', kinds: [] },
];

let seq = 0;
const uid = (prefix) => `${prefix}-${Date.now().toString(36)}-${(++seq).toString(36)}`;

/** A new field of `kind`, with that kind's default name and secrecy. */
export function newField(kind, value = '') {
  const k = kindInfo(kind);
  return { id: uid('f'), kind: k.kind, name: k.name, secret: k.secret, value: String(value) };
}

export function templateFields(templateId) {
  return (VAULT_TEMPLATES.find((t) => t.id === templateId) ?? VAULT_TEMPLATES[0]).kinds.map((k) => newField(k));
}

/** Copies of `fields` with fresh ids (for a duplicated entry). */
export const cloneFields = (fields) => (fields ?? []).map((f) => ({ ...f, id: uid('f') }));

/** An entry in the current shape. The first shape (fixed username / password / url)
 *  becomes fields; `migrated` tells whether it had to. */
export function normalizeEntry(e) {
  const now = Date.now();
  let fields;
  const migrated = !Array.isArray(e?.fields);
  if (!migrated) {
    fields = e.fields.map((f) => {
      const k = kindInfo(f?.kind);
      return {
        id: f?.id || uid('f'),
        kind: k.kind,
        name: typeof f?.name === 'string' ? f.name : k.name,
        secret: typeof f?.secret === 'boolean' ? f.secret : k.secret,
        value: String(f?.value ?? ''),
      };
    });
  } else {
    const kinds = ['username', 'password', 'url'].filter((k) => e?.[k]);
    fields = kinds.length ? kinds.map((k) => newField(k, e[k])) : templateFields('login');
  }
  return {
    migrated,
    entry: {
      id: e?.id || uid('vault'),
      label: String(e?.label ?? ''),
      role: String(e?.role ?? ''),
      note: String(e?.note ?? ''),
      fields,
      createdAt: e?.createdAt ?? now,
      updatedAt: e?.updatedAt ?? e?.createdAt ?? now,
    },
  };
}

/** First non-empty value among the entry's fields of these kinds. */
export function valueOf(e, kinds) {
  return (e?.fields ?? []).find((f) => kinds.includes(f.kind) && String(f.value).trim())?.value ?? '';
}

/** The login identity: the first username, else the first email. */
export const identityOf = (e) => valueOf(e, ['username', 'email']);

/** `https://` is assumed when the URL has no scheme; empty stays empty. */
export function normalizeUrl(url) {
  const u = String(url ?? '').trim();
  if (!u) return '';
  return /^[a-z][a-z0-9+.-]*:\/\//i.test(u) ? u : `https://${u}`;
}

/** Host of a URL for display (`staging.example.com:8443`), or the text as typed. */
export function hostOf(url) {
  const u = normalizeUrl(url);
  if (!u) return '';
  try {
    return new URL(u).host;
  } catch {
    return String(url).trim();
  }
}

/** What a credential is called in the list: label, else username / email, else the
 *  URL's host, else the first custom value. */
export function vaultName(e) {
  return (
    e?.label?.trim() ||
    identityOf(e).trim() ||
    hostOf(valueOf(e, ['url'])) ||
    valueOf(e, ['text']).trim() ||
    'Untitled credential'
  );
}

/** The list's second line: what the name doesn't already show (identity, host), else
 *  the secrets it holds (e.g. "API key"), else "Empty". Never a secret value. */
export function vaultSubtitle(e) {
  const user = identityOf(e).trim();
  const host = hostOf(valueOf(e, ['url']));
  const parts = e?.label?.trim() ? [user, host] : user ? [host] : [];
  const shown = parts.filter(Boolean).join(' · ');
  if (shown) return shown;
  const secrets = [...new Set((e?.fields ?? []).filter((f) => f.secret && String(f.value).trim()).map((f) => f.name.trim() || kindInfo(f.kind).name))];
  return secrets.length ? secrets.join(' · ') : 'Empty';
}

const sortKey = (e) => (e.label?.trim() || identityOf(e).trim() || hostOf(valueOf(e, ['url'])) || '').toLowerCase();

/** Entries matching `query` (label, role, note, field names, non-secret values; never a
 *  secret value), ordered by name. Unnamed entries (just added) come first. */
export function vaultView(entries, query = '') {
  const q = String(query ?? '').trim().toLowerCase();
  const hit = (v) => String(v ?? '').toLowerCase().includes(q);
  return (entries ?? [])
    .filter((e) => !q || hit(e.label) || hit(e.role) || hit(e.note) || (e.fields ?? []).some((f) => hit(f.name) || (!f.secret && hit(f.value))))
    .sort((a, b) => {
      const x = sortKey(a);
      const y = sortKey(b);
      if (!x !== !y) return x ? 1 : -1;
      return x.localeCompare(y, undefined, { numeric: true }) || (a.createdAt ?? 0) - (b.createdAt ?? 0);
    });
}

/** `Authorization: Basic …` for user:pass (UTF-8 safe), ready to paste into a request. */
export function basicAuthHeader(user, pass) {
  const bytes = new TextEncoder().encode(`${user ?? ''}:${pass ?? ''}`);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return `Authorization: Basic ${btoa(bin)}`;
}

/** Basic auth header for an entry with a username / email or a password, else ''. */
export function basicFor(e) {
  const user = identityOf(e);
  const pass = valueOf(e, ['password']);
  return user || pass ? basicAuthHeader(user, pass) : '';
}

/** `Authorization: Bearer …` for an entry's first token, else ''. */
export function bearerFor(e) {
  const token = valueOf(e, ['token']).trim();
  return token ? `Authorization: Bearer ${token}` : '';
}

// Highlight colours (see colors.js / app.css .c-*) minus gray, for avatars.
const AVATAR_COLORS = ['red', 'orange', 'yellow', 'green', 'cyan', 'blue', 'purple', 'pink'];

/** A stable colour per entry (from its id), so it doesn't change while typing. */
export function avatarColor(id) {
  let h = 0;
  for (const ch of String(id ?? '')) h = (h * 31 + ch.charCodeAt(0)) | 0;
  return AVATAR_COLORS[Math.abs(h) % AVATAR_COLORS.length];
}
