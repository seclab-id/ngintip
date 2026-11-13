import { brandKey } from './brand.js';

/**
 * Remembered "which version am I looking at" choices for the message panes.
 *
 * Each pane keeps its own: a request and a response are different messages with
 * different versions available, and wanting the decoded request says nothing about
 * wanting the decoded response. Persisted, so the choice survives a restart the way the
 * Organizer's tab does.
 *
 * Every choice carries its own fallback, and [`Choice.openOn`] applies it when the
 * remembered option is not among the ones this message actually offers. That rule is why
 * a preference can never show a version that does not exist, or quietly hide the wire
 * behind a decode on a message that was never decoded.
 */
class Choice {
  #key;
  #allowed;
  #fallback;
  /** The remembered value. Reactive, so a pane re-renders when another one changes it. */
  current = $state('');

  constructor(key, allowed, fallback) {
    this.#key = brandKey(key);
    this.#allowed = allowed;
    this.#fallback = fallback;
    this.current = this.#load();
  }

  #load() {
    try {
      const saved = localStorage.getItem(this.#key);
      return this.#allowed.includes(saved) ? saved : this.#fallback;
    } catch {
      // Storage unavailable (private window, blocked site data): use the safe default.
      return this.#fallback;
    }
  }

  /** Remember a choice. Choosing the fallback is a choice too, and is remembered. */
  pick(id) {
    if (!this.#allowed.includes(id)) return;
    this.current = id;
    try {
      localStorage.setItem(this.#key, id);
    } catch {
      // Not persisted, but it still holds for this session.
    }
  }

  /** Which option to open on, given the ids this message actually offers. */
  openOn(available) {
    return Array.isArray(available) && available.includes(this.current) ? this.current : this.#fallback;
  }
}

/** Version ids the History/Intruder/Organizer switcher uses (see buildVariants). */
export const VARIANT_IDS = ['final', 'decoded', 'original'];

/**
 * The message viewer's switcher. The wire (`final`) is the fallback and the first-run
 * default: a security tool must not hide what actually crossed the wire behind a decode
 * before anyone has asked it to.
 */
export const variantPref = {
  req: new Choice('message.variant.req', VARIANT_IDS, 'final'),
  res: new Choice('message.variant.res', VARIANT_IDS, 'final'),
};

/**
 * Repeater's own segments, which have their own vocabulary and their own defaults.
 *
 * The defaults are not the wire here, and deliberately so: `typed` is the request you can
 * edit, and `final` is the reply after an extension made it readable. Those are the ones
 * you work in. This only remembers the choice; it does not change which is default.
 */
export const repeaterPref = {
  req: new Choice('repeater.requestView', ['typed', 'sent'], 'typed'),
  res: new Choice('repeater.responseView', ['final', 'original'], 'final'),
};
