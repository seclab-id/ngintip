// Line-level diff (LCS) for the Comparer: compare two HTTP messages (or any text)
// and show which lines were added / removed / unchanged.

function splitLines(t) {
  return (t ?? '').replace(/\r\n/g, '\n').split('\n');
}

// The comparison key for a line, given the match options. The original text is
// always what gets displayed; only equality uses this key.
function keyOf(line, opts) {
  let k = line;
  if (opts.ignoreCase) k = k.toLowerCase();
  if (opts.ignoreWhitespace) k = k.replace(/\s+/g, ' ').trim();
  return k;
}

/**
 * LCS diff over the key arrays (ka/kb), carrying the ORIGINAL text (a/b) into the
 * ops -> ordered [{ type: 'same'|'add'|'del', text }].
 */
function lcsDiff(a, b, ka, kb) {
  const n = a.length;
  const m = b.length;
  if (!n && !m) return [];
  if (!n) return b.map((text) => ({ type: 'add', text }));
  if (!m) return a.map((text) => ({ type: 'del', text }));
  const w = m + 1;
  const dp = new Int32Array((n + 1) * w);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      dp[i * w + j] = ka[i] === kb[j] ? dp[(i + 1) * w + (j + 1)] + 1 : Math.max(dp[(i + 1) * w + j], dp[i * w + (j + 1)]);
    }
  }
  const ops = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (ka[i] === kb[j]) {
      ops.push({ type: 'same', text: a[i] });
      i++;
      j++;
    } else if (dp[(i + 1) * w + j] >= dp[i * w + (j + 1)]) {
      ops.push({ type: 'del', text: a[i] });
      i++;
    } else {
      ops.push({ type: 'add', text: b[j] });
      j++;
    }
  }
  while (i < n) ops.push({ type: 'del', text: a[i++] });
  while (j < m) ops.push({ type: 'add', text: b[j++] });
  return ops;
}

// Fallback for very large inputs: don't run the O(n*m) table, just show every left
// line removed then every right line added (still correct, only less minimal).
function naiveDiff(a, b) {
  return [...a.map((text) => ({ type: 'del', text })), ...b.map((text) => ({ type: 'add', text }))];
}

const MAX_CELLS = 4_000_000; // ~16 MB Int32 table cap

/**
 * Compare two texts line by line. Returns ops in order, each
 * `{ type: 'same'|'add'|'del', text, a, b }` where `a`/`b` are 1-based line
 * numbers on each side (null when the line is absent there). `opts` controls how
 * lines are matched: `{ ignoreCase, ignoreWhitespace }` (display is unchanged).
 */
export function lineDiff(aText, bText, opts = {}) {
  const a = splitLines(aText);
  const b = splitLines(bText);
  const ka = a.map((l) => keyOf(l, opts));
  const kb = b.map((l) => keyOf(l, opts));
  const n = a.length;
  const m = b.length;

  // Trim the common prefix / suffix so the LCS table only spans what actually
  // differs (keeps big, mostly-identical messages fast and bounded).
  let lo = 0;
  while (lo < n && lo < m && ka[lo] === kb[lo]) lo++;
  let hiA = n;
  let hiB = m;
  while (hiA > lo && hiB > lo && ka[hiA - 1] === kb[hiB - 1]) {
    hiA--;
    hiB--;
  }

  const ops = [];
  for (let i = 0; i < lo; i++) ops.push({ type: 'same', text: a[i] });
  const midA = a.slice(lo, hiA);
  const midB = b.slice(lo, hiB);
  const mid =
    midA.length * midB.length > MAX_CELLS
      ? naiveDiff(midA, midB)
      : lcsDiff(midA, midB, ka.slice(lo, hiA), kb.slice(lo, hiB));
  for (const op of mid) ops.push(op);
  for (let i = hiA; i < n; i++) ops.push({ type: 'same', text: a[i] });

  // Assign per-side line numbers.
  let ai = 0;
  let bi = 0;
  return ops.map((op) => {
    if (op.type === 'same') return { ...op, a: ++ai, b: ++bi };
    if (op.type === 'del') return { ...op, a: ++ai, b: null };
    return { ...op, a: null, b: ++bi };
  });
}

/** Count of added / removed / unchanged lines for a diff op list. */
export function diffStats(ops) {
  let added = 0;
  let removed = 0;
  let same = 0;
  for (const op of ops) {
    if (op.type === 'add') added++;
    else if (op.type === 'del') removed++;
    else same++;
  }
  return { added, removed, same, changed: added + removed };
}
