export function fmtSize(n) {
  if (!n) return '0 B';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function fmtTime(ts) {
  return new Date(ts).toLocaleTimeString([], { hour12: false });
}

/** "2026-09-23 15:04:05" for tooltips and Repeater history. */
export function fmtDateTime(ts) {
  const d = new Date(ts);
  const p = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

export function statusClass(status) {
  if (!status) return 's-err';
  if (status < 300) return 's-2xx';
  if (status < 400) return 's-3xx';
  if (status < 500) return 's-4xx';
  return 's-5xx';
}

/** A wait, worded for a badge: "8s", "1m 14s", "2h 3m". */
export function fmtWaited(ms) {
  const t = Math.max(0, Math.round(ms / 1000));
  if (t < 60) return `${t}s`;
  if (t < 3600) return `${Math.floor(t / 60)}m ${t % 60}s`;
  return `${Math.floor(t / 3600)}h ${Math.floor((t % 3600) / 60)}m`;
}

/**
 * How long a wait runs before History stops calling it ordinary. It changes the
 * badge and nothing else: no row is rewritten, no error is stored, and a response
 * that arrives after it still lands normally. That is the whole reason a number
 * picked out of the air is safe here. Pick it wrong and an operator sees a word
 * they disagree with; it can never cost evidence.
 */
export const LATE_AFTER_MS = 60_000;

/**
 * What a row IS right now, which is a different question from what status it holds.
 *
 * Five states share "no status code", and flattening them is what made a healthy
 * request in flight look like a dead one. Only the last is a failure, and only the
 * last is red:
 *
 *   wait   - out at the target, this process is still waiting. Nothing has failed.
 *   late   - the same, for longer than anyone expected. Still nothing has failed.
 *   unsent - the client gave up while Ngintip still held the request, so the target
 *            never saw it. There is no exchange with the target to read at all.
 *   noresp - it did reach the target, but no answer was ever filed. A gap in the
 *            record, not a verdict on the target, which may well have answered an
 *            operator who had gone.
 *   err    - something was observed to fail, and `error` says what.
 *
 * `now` is passed in so a table full of waiting rows repaints off one clock instead
 * of every row keeping its own, and `lateAfterMs` because the operator sets it
 * (Settings > Project > Performance). 0 means never escalate.
 */
export function flowState(f, now = Date.now(), lateAfterMs = LATE_AFTER_MS) {
  if (f.status) return { kind: 'done', cls: statusClass(f.status), badge: String(f.status), title: '' };

  if (f.pending) {
    const waited = Math.max(0, now - f.ts);
    // 0 turns the escalation off: the operator has said a long wait here is normal,
    // so the row keeps saying the one thing that is certainly true, that it is waiting.
    const late = lateAfterMs > 0 && waited >= lateAfterMs;
    return {
      kind: late ? 'late' : 'wait',
      cls: late ? 's-late' : 's-wait',
      badge: late ? 'LATE' : 'WAIT',
      waited,
      // Said about the wait, never about the target: nothing has been observed
      // about the target at all, which is exactly why this is not an error.
      title: late
        ? `Still waiting after ${fmtWaited(waited)}. Nothing has failed. Ngintip sets no deadline of its own and will keep waiting as long as the client does.`
        : 'Waiting for the response. Nothing has failed.',
    };
  }

  if (!f.error) {
    // Nested inside "nothing was observed to fail" on purpose. `unsent` is a flag this
    // process writes, `error` is something that was watched happening, and a watched
    // failure must always outrank a flag: the two cannot both be true today, and if a
    // later writer ever made them so, hiding the failure would be the worse answer.
    if (f.unsent) {
      return {
        kind: 'unsent',
        cls: 's-noresp',
        badge: 'UNSENT',
        // Not red: nothing failed. The request simply never got to the target, so the
        // row is a record of what the client asked for, not evidence about the target.
        title: 'The client gave up before this request was handed to the target, so the target never saw it. Kept as a record of what the client asked for, not as evidence about the target.',
      };
    }
    return {
      kind: 'noresp',
      cls: 's-noresp',
      badge: 'NO RSP',
      // Amber, not red, and worded so it cannot be written up as a target failure.
      title: 'No answer was ever filed for this request. That is a gap in this record, not a failure of the target, which may have answered normally.',
    };
  }

  return { kind: 'err', cls: 's-err', badge: 'ERR', title: f.error };
}

export function methodClass(method) {
  return `m-${method.toLowerCase()}`;
}
