import { describe, expect, it } from 'vitest';
import { compileRules, ruleError, scanFlow, scanResponseHeaders, scanText, flowInScope, parseScope, DEFAULT_PASSIVE_SCOPE } from './passive.js';
import { BUILTIN_RULES } from './passive-rules.js';

const types = (arr) => arr.map((f) => f.type);

describe('passive scanner', () => {
  it('detects secrets and tokens in text', () => {
    const jwt = 'token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c';
    expect(types(scanText(jwt, 'response'))).toContain('jwt');
    expect(types(scanText('AKIAIOSFODNN7EXAMPLE', 'response'))).toContain('aws-key');
    expect(types(scanText('key=AIza' + 'A'.repeat(35), 'response'))).toContain('google-key');
    expect(types(scanText('"api_key": "s3cr3tvalue123"', 'response'))).toContain('secret-assign');
    expect(types(scanText('-----BEGIN RSA PRIVATE KEY-----', 'response'))).toContain('private-key');
    expect(types(scanText('contact me at admin@example.com', 'response'))).toContain('email');
  });

  it('email matches real addresses but not the common false positives', () => {
    const emails = (t) => scanText(t, 'response').filter((f) => f.type === 'email').map((f) => f.match);
    // Real addresses (various positions/quoting).
    expect(emails('reach me: admin@example.com')).toEqual(['admin@example.com']);
    expect(emails('"support@sub.example.co.uk"')).toEqual(['support@sub.example.co.uk']);
    expect(emails('mailto:jane.doe+tag@corp.io')).toEqual(['jane.doe+tag@corp.io']);
    // False positives that must NOT match.
    expect(emails('background:url(logo@2x.png)')).toEqual([]); // retina asset ref
    expect(emails('img srcset="a@3x.jpg 3x"')).toEqual([]);
    expect(emails('https://publickey@o12345.ingest.sentry.io/1')).toEqual([]); // URL userinfo
    expect(emails('build@1.2.3 and v@4.5')).toEqual([]); // version-ish
    expect(emails('react@18.2.0')).toEqual([]);
    // Code/asset module refs whose extension only looks like a TLD.
    expect(emails("import x from 'main@bundle.js'")).toEqual([]);
    expect(emails('theme@dark.css and comp@button.vue')).toEqual([]);
    expect(emails('vendor@app.min.js')).toEqual([]);
    expect(emails('handler@main.go')).toEqual([]);
    // …but a real address on a genuine TLD still matches.
    expect(emails('dev@team.dev')).toEqual(['dev@team.dev']);
    // Garbage local parts (consecutive/leading/trailing dots) are not emails.
    expect(emails('scan hit 6..@qA.AR here')).toEqual([]);
    expect(emails('a..b@x.com')).toEqual([]);
    expect(emails('.a@x.com')).toEqual([]);
    expect(emails('a.@x.com')).toEqual([]);
  });

  it('ranks version-bearing tech headers above bare product names', () => {
    const sev = (line) => {
      const raw = `HTTP/1.1 200 OK\n${line}\n\n{}`;
      return scanResponseHeaders(raw).find((f) => f.type === 'tech')?.severity;
    };
    expect(sev('Server: nginx/1.25.3')).toBe('low');
    expect(sev('X-Powered-By: PHP/8.2')).toBe('low');
    expect(sev('X-AspNet-Version: 4.0.30319')).toBe('low');
    expect(sev('Server: cloudflare')).toBe('info');
    expect(sev('Server: Varnish')).toBe('info');
  });

  it('detects bundler-exposed and cloud provider secrets', () => {
    expect(types(scanText('VITE_API_KEY = "sk_live_abcd1234efgh"', 'response'))).toContain('vite-exposed-secret');
    expect(types(scanText('NEXT_PUBLIC_STRIPE_SECRET="pk_test_99887766554433"', 'response'))).toContain('vite-exposed-secret');
    expect(types(scanText('AccountKey=' + 'A'.repeat(86) + '==', 'response'))).toContain('azure-storage-key');
    expect(types(scanText('DefaultEndpointsProtocol=https;AccountName=store;AccountKey=' + 'B'.repeat(88), 'response'))).toContain('azure-storage-conn');
    expect(types(scanText('{ "type": "service_account" }', 'response'))).toContain('gcp-service-account');
    expect(types(scanText('GOCSPX-' + 'a'.repeat(28), 'response'))).toContain('google-oauth-client-secret');
    expect(types(scanText('github_pat_' + 'A'.repeat(82), 'response'))).toContain('github-fine-grained-pat');
    expect(types(scanText('glpat-' + 'x'.repeat(20), 'response'))).toContain('gitlab-pat');
    expect(types(scanText('dop_v1_' + 'a'.repeat(64), 'response'))).toContain('digitalocean-token');
    // Ordinary code doesn't trip the bundler rule (no quoted value assigned).
    expect(types(scanText('const VITE_MODE = mode;', 'response'))).not.toContain('vite-exposed-secret');
  });

  it('detects the additional provider tokens', () => {
    const hit = (t, id) => types(scanText(t, 'response')).includes(id);
    expect(hit('xapp-1-A012BC-1112131415-abcdef012345', 'slack-app-token')).toBe(true);
    expect(hit('123456789:AA' + 'a'.repeat(33), 'telegram-bot-token')).toBe(true);
    expect(hit('M' + 'a'.repeat(23) + '.' + 'b'.repeat(6) + '.' + 'c'.repeat(27), 'discord-bot-token')).toBe(true);
    expect(hit('sq0atp-' + 'a'.repeat(22), 'square-access-token')).toBe(true);
    expect(hit('sq0csp-' + 'a'.repeat(43), 'square-oauth-secret')).toBe(true);
    expect(hit('pypi-AgEIcHlwaS5vcmc' + 'a'.repeat(50), 'pypi-token')).toBe(true);
    expect(hit('PMAK-' + 'a'.repeat(24) + '-' + 'b'.repeat(34), 'postman-key')).toBe(true);
    expect(hit('lin_api_' + 'a'.repeat(40), 'linear-key')).toBe(true);
    expect(hit('dapi' + 'a'.repeat(32), 'databricks-token')).toBe(true);
    expect(hit('figd_' + 'a'.repeat(40), 'figma-token')).toBe(true);
    expect(hit('dt0c01.' + 'A'.repeat(24) + '.' + 'A'.repeat(64), 'dynatrace-token')).toBe(true);
    expect(hit('hvs.' + 'a'.repeat(24), 'vault-token')).toBe(true);
    expect(hit('cloudinary://123456789012:abcDEF_secret@mycloud', 'cloudinary-url')).toBe(true);
    expect(hit('AGE-SECRET-KEY-1' + 'A'.repeat(58), 'age-secret-key')).toBe(true);
    // Ordinary prose mentioning these products does not trip the rules.
    expect(hit('the vault was empty', 'vault-token')).toBe(false);
    expect(hit('cloudinary integration guide', 'cloudinary-url')).toBe(false);
    expect(hit('linear algebra homework', 'linear-key')).toBe(false);
  });

  it('does not flag ordinary text', () => {
    expect(scanText('the quick brown fox jumps over the lazy dog', 'response')).toEqual([]);
    expect(scanText('', 'response')).toEqual([]);
  });

  it('flags risky response headers', () => {
    const raw = [
      'HTTP/1.1 200 OK',
      'Server: nginx/1.25.3',
      'X-Powered-By: PHP/8.2',
      'Access-Control-Allow-Origin: *',
      'Set-Cookie: sid=abc; Path=/',
      '',
      '{}',
    ].join('\n');
    const t = types(scanResponseHeaders(raw));
    expect(t).toContain('tech'); // Server + X-Powered-By
    expect(t).toContain('cors');
    expect(t).toContain('cookie'); // missing HttpOnly & Secure
  });

  it('does not flag a hardened cookie', () => {
    const raw = 'HTTP/1.1 200 OK\nSet-Cookie: sid=abc; HttpOnly; Secure\n\n';
    expect(types(scanResponseHeaders(raw))).not.toContain('cookie');
  });

  it('scanFlow merges request+response and dedupes', () => {
    const responseRaw = 'HTTP/1.1 200 OK\nAccess-Control-Allow-Origin: *\n\nadmin@example.com admin@example.com';
    const found = scanFlow({ requestRaw: '', responseRaw });
    // The duplicate email collapses to one finding.
    expect(found.filter((f) => f.type === 'email').length).toBe(1);
    expect(types(found)).toContain('cors');
  });

  it('scopes scanning by extension and content-type', () => {
    const scope = ['js', 'json', 'html', 'css'];
    // Path extension in scope.
    expect(flowInScope({ path: '/assets/main.abc.js' }, scope)).toBe(true);
    expect(flowInScope({ path: '/style.css?v=2' }, scope)).toBe(true);
    // Extension-less API path, but its content-type is in scope.
    expect(flowInScope({ path: '/api/users', mime: 'json' }, scope)).toBe(true);
    expect(flowInScope({ path: '/api/users', mime: 'application/json' }, scope)).toBe(true);
    // Out of scope.
    expect(flowInScope({ path: '/logo.png', mime: 'png' }, scope)).toBe(false);
    expect(flowInScope({ path: '/f.woff2', mime: 'font/woff2' }, scope)).toBe(false);
    // Empty scope / '*' → everything.
    expect(flowInScope({ path: '/logo.png', mime: 'png' }, [])).toBe(true);
    expect(flowInScope({ path: '/logo.png', mime: 'png' }, ['*'])).toBe(true);
  });

  it('parseScope normalizes user input', () => {
    expect(parseScope('js, .json  CSS')).toEqual(['js', 'json', 'css']);
    expect(parseScope('.js, js')).toEqual(['js']); // dedup + strip dots
    expect(parseScope('js, *, css')).toEqual(['*']); // any '*' collapses to all
    expect(parseScope('')).toEqual([]);
    expect(DEFAULT_PASSIVE_SCOPE).toContain('js');
    expect(DEFAULT_PASSIVE_SCOPE).toContain('json');
  });

  it('all built-in rules compile', () => {
    expect(compileRules(BUILTIN_RULES).length).toBe(BUILTIN_RULES.length);
  });

  it('ruleError validates a pattern', () => {
    expect(ruleError({ pattern: '\\d+', flags: 'i' })).toBe('');
    expect(ruleError({ pattern: '(' })).not.toBe('');
    expect(ruleError({ pattern: '' })).toBe('Pattern is required');
  });

  it('custom compiled rules are honored and use the rule id/group', () => {
    const compiled = compileRules([{ id: 'ticket', title: 'Ticket', severity: 'low', pattern: 'JIRA-(\\d+)', flags: 'g', group: 1 }]);
    const found = scanText('see JIRA-1234 for details', 'response', compiled);
    expect(found).toEqual([{ type: 'ticket', severity: 'low', title: 'Ticket', where: 'response', match: '1234' }]);
    // An invalid rule is skipped, not thrown.
    expect(compileRules([{ id: 'bad', pattern: '(' }]).length).toBe(0);
  });
});
