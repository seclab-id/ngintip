import { describe, expect, it } from 'vitest';
import { advActive, BASIC_EXCLUDE_FILTERS, emptyAdvanced, matchAdvanced, newFilter, statusKey, urlExtension } from './histfilter.js';

const flow = (o) => ({ method: 'GET', status: 200, host: 'api.example.com', path: '/v1/users?id=1', mime: 'application/json', reqLength: 0, length: 42, durationMs: 12, ...o });

describe('histfilter', () => {
  it('statusKey buckets by class, 0/odd -> err', () => {
    expect(statusKey(204)).toBe('2');
    expect(statusKey(301)).toBe('3');
    expect(statusKey(404)).toBe('4');
    expect(statusKey(500)).toBe('5');
    expect(statusKey(0)).toBe('err');
  });

  it('an empty filter is inactive and matches everything', () => {
    const a = emptyAdvanced();
    expect(advActive(a)).toBe(false);
    expect(matchAdvanced(flow(), a)).toBe(true);
  });

  it('filters by method and status class', () => {
    const a = { ...emptyAdvanced(), methods: ['POST', 'PUT'] };
    expect(advActive(a)).toBe(true);
    expect(matchAdvanced(flow({ method: 'POST' }), a)).toBe(true);
    expect(matchAdvanced(flow({ method: 'GET' }), a)).toBe(false);

    const s = { ...emptyAdvanced(), status: ['4', 'err'] };
    expect(matchAdvanced(flow({ status: 401 }), s)).toBe(true);
    expect(matchAdvanced(flow({ status: 0 }), s)).toBe(true);
    expect(matchAdvanced(flow({ status: 200 }), s)).toBe(false);
  });

  it('filter rules can be case sensitive or insensitive', () => {
    const f = flow({ path: '/v1/Users' });
    // Insensitive (default): matches regardless of case.
    const insens = { ...emptyAdvanced(), filters: [{ field: 'path', mode: 'include', value: '/v1/users', sensitive: false }] };
    expect(matchAdvanced(f, insens)).toBe(true);
    // Sensitive: only an exact-case substring matches.
    const sens = { ...emptyAdvanced(), filters: [{ field: 'path', mode: 'include', value: '/v1/users', sensitive: true }] };
    expect(matchAdvanced(f, sens)).toBe(false);
    expect(matchAdvanced(flow({ path: '/v1/users' }), sens)).toBe(true);
  });

  it('applies multiple include/exclude filter rules', () => {
    const f = (o) => flow(o);
    // Exclude rules are a denylist: any match hides the flow.
    const exc = {
      ...emptyAdvanced(),
      filters: [
        { field: 'path', mode: 'exclude', value: '/static' },
        { field: 'path', mode: 'exclude', value: '.png' },
        { field: 'host', mode: 'exclude', value: 'cdn.' },
      ],
    };
    expect(matchAdvanced(f({ path: '/static/app.js' }), exc)).toBe(false);
    expect(matchAdvanced(f({ path: '/img/logo.png' }), exc)).toBe(false);
    expect(matchAdvanced(f({ host: 'cdn.example.com', path: '/x' }), exc)).toBe(false);
    expect(matchAdvanced(f({ path: '/api/users' }), exc)).toBe(true);

    // Include rules are an allowlist (OR): must match at least one.
    const inc = {
      ...emptyAdvanced(),
      filters: [
        { field: 'path', mode: 'include', value: '/api' },
        { field: 'method', mode: 'include', value: 'POST' },
      ],
    };
    expect(matchAdvanced(f({ path: '/api/users', method: 'GET' }), inc)).toBe(true);
    expect(matchAdvanced(f({ path: '/x', method: 'POST' }), inc)).toBe(true);
    expect(matchAdvanced(f({ path: '/x', method: 'GET' }), inc)).toBe(false);

    // Mixed: include allowlist AND exclude denylist together.
    const mix = {
      ...emptyAdvanced(),
      filters: [
        { field: 'path', mode: 'include', value: '/api' },
        { field: 'path', mode: 'exclude', value: '/api/health' },
      ],
    };
    expect(matchAdvanced(f({ path: '/api/users' }), mix)).toBe(true);
    expect(matchAdvanced(f({ path: '/api/health' }), mix)).toBe(false);
    expect(matchAdvanced(f({ path: '/other' }), mix)).toBe(false);

    // A rule with a blank value is ignored; advActive reflects real rules.
    expect(advActive({ ...emptyAdvanced(), filters: [{ field: 'path', mode: 'exclude', value: '' }] })).toBe(false);
    expect(advActive(exc)).toBe(true);
  });

  it('a disabled rule is ignored (mute instead of delete)', () => {
    const f = (o) => flow(o);
    // One exclude rule, muted via enabled:false -> acts as if it were not there.
    const muted = { ...emptyAdvanced(), filters: [{ field: 'path', mode: 'exclude', value: '/static', enabled: false }] };
    expect(advActive(muted)).toBe(false);
    expect(matchAdvanced(f({ path: '/static/app.js' }), muted)).toBe(true);

    // A missing enabled flag counts as on (backward compatible with saved rules).
    const legacy = { ...emptyAdvanced(), filters: [{ field: 'path', mode: 'exclude', value: '/static' }] };
    expect(advActive(legacy)).toBe(true);
    expect(matchAdvanced(f({ path: '/static/app.js' }), legacy)).toBe(false);

    // Disabling every rule shows all flows again without losing the rules.
    const allOff = {
      ...emptyAdvanced(),
      filters: [
        { field: 'path', mode: 'include', value: '/api', enabled: false },
        { field: 'path', mode: 'exclude', value: '/api/health', enabled: false },
      ],
    };
    expect(advActive(allOff)).toBe(false);
    expect(matchAdvanced(f({ path: '/other' }), allOff)).toBe(true);
    expect(matchAdvanced(f({ path: '/api/health' }), allOff)).toBe(true);

    // A disabled include rule does not force the allowlist; only the enabled one does.
    const partial = {
      ...emptyAdvanced(),
      filters: [
        { field: 'path', mode: 'include', value: '/api', enabled: false },
        { field: 'method', mode: 'include', value: 'POST', enabled: true },
      ],
    };
    expect(matchAdvanced(f({ path: '/api/users', method: 'GET' }), partial)).toBe(false);
    expect(matchAdvanced(f({ path: '/x', method: 'POST' }), partial)).toBe(true);
  });

  it('urlExtension reads the real extension, ignoring query and path', () => {
    expect(urlExtension('/assets/app.min.js?v=2')).toBe('js');
    expect(urlExtension('/data.json')).toBe('json');
    expect(urlExtension('/app.js.map')).toBe('map');
    expect(urlExtension('/IMG/Logo.PNG')).toBe('png'); // lowercased
    expect(urlExtension('/api/v1/users')).toBe(''); // no extension
    expect(urlExtension('/download?file=logo.png')).toBe(''); // extension only in the query is ignored
    expect(urlExtension('/')).toBe('');
  });

  it('the ext field matches a file extension exactly (js excludes app.js, not data.json)', () => {
    const a = { ...emptyAdvanced(), filters: [newFilter({ field: 'ext', mode: 'exclude', value: 'js' })] };
    expect(matchAdvanced(flow({ path: '/assets/app.js' }), a)).toBe(false);
    expect(matchAdvanced(flow({ path: '/assets/app.min.js?v=2' }), a)).toBe(false); // query ignored
    expect(matchAdvanced(flow({ path: '/config.json' }), a)).toBe(true); // the whole point: .json stays
    expect(matchAdvanced(flow({ path: '/app.json?cb=1' }), a)).toBe(true);
    // A leading dot in the value is accepted and still means exact.
    const dotted = { ...emptyAdvanced(), filters: [newFilter({ field: 'ext', mode: 'exclude', value: '.JS' })] };
    expect(matchAdvanced(flow({ path: '/a.js' }), dotted)).toBe(false);
    expect(matchAdvanced(flow({ path: '/a.json' }), dotted)).toBe(true);
  });

  it('BASIC_EXCLUDE_FILTERS hides static junk by extension but keeps .json API traffic', () => {
    const a = { ...emptyAdvanced(), filters: BASIC_EXCLUDE_FILTERS.map((p) => newFilter({ ...p, mode: 'exclude' })) };
    expect(advActive(a)).toBe(true);
    // Static assets are hidden by their extension.
    expect(matchAdvanced(flow({ path: '/assets/app.js?v=2' }), a)).toBe(false);
    expect(matchAdvanced(flow({ path: '/assets/app.min.js' }), a)).toBe(false);
    expect(matchAdvanced(flow({ path: '/esm/module.mjs' }), a)).toBe(false);
    expect(matchAdvanced(flow({ path: '/style.css' }), a)).toBe(false);
    expect(matchAdvanced(flow({ path: '/logo.png' }), a)).toBe(false);
    expect(matchAdvanced(flow({ path: '/f.woff2' }), a)).toBe(false);
    expect(matchAdvanced(flow({ path: '/app.js.map' }), a)).toBe(false); // source map
    // JSON API traffic stays visible (the whole point).
    expect(matchAdvanced(flow({ path: '/config.json' }), a)).toBe(true);
    expect(matchAdvanced(flow({ path: '/api/v1/users' }), a)).toBe(true);
    expect(matchAdvanced(flow({ path: '/index.html' }), a)).toBe(true);
  });

  it('filters by request/response body presence and duration range', () => {
    expect(matchAdvanced(flow({ reqLength: 0 }), { ...emptyAdvanced(), reqBody: 'yes' })).toBe(false);
    expect(matchAdvanced(flow({ reqLength: 10 }), { ...emptyAdvanced(), reqBody: 'yes' })).toBe(true);
    expect(matchAdvanced(flow({ length: 0 }), { ...emptyAdvanced(), resBody: 'no' })).toBe(true);
    expect(matchAdvanced(flow({ length: 5 }), { ...emptyAdvanced(), resBody: 'no' })).toBe(false);

    expect(matchAdvanced(flow({ durationMs: 12 }), { ...emptyAdvanced(), minMs: '20' })).toBe(false);
    expect(matchAdvanced(flow({ durationMs: 50 }), { ...emptyAdvanced(), minMs: '20', maxMs: '100' })).toBe(true);
    expect(matchAdvanced(flow({ durationMs: 200 }), { ...emptyAdvanced(), maxMs: '100' })).toBe(false);
  });

  it('combines fields with AND', () => {
    const a = { ...emptyAdvanced(), methods: ['GET'], status: ['2'], mime: 'json' };
    expect(matchAdvanced(flow(), a)).toBe(true);
    expect(matchAdvanced(flow({ status: 404 }), a)).toBe(false);
  });
});
