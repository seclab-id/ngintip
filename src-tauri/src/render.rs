//! Asking the window to draw something, and waiting for the answer.
//!
//! The evidence card that `history_image` returns is drawn by `src/lib/capture.js` on a
//! Canvas 2D context, which only exists in the webview. That renderer measures real font
//! metrics, paints redactions over pixels and colours body tokens from two palettes that
//! mirror `app.css`. Reimplementing it here would mean two renderers for one card shape,
//! and the card an agent sees would stop matching the card the operator sees, although
//! both can end up in the same report.
//!
//! So the request goes out as an event and the PNG comes back as a command. This type is
//! the waiting room in between: one id per request, one oneshot per id, and an entry that
//! is always removed, whether the answer arrives, fails or never comes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::oneshot;

/// How long to wait for the window. Long enough for a huge body on a busy webview, short
/// enough that a caller is told rather than left hanging.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// What the window sends back: the PNG bytes, or the reason it could not draw them.
pub type Answer = Result<Vec<u8>, String>;

#[derive(Default)]
pub struct Bridge {
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
    /// Overridden in tests so a timeout test does not take 20 seconds.
    timeout_ms: AtomicU64,
}

impl Bridge {
    /// Take a ticket. The id goes to the window; the receiver stays here.
    pub fn begin(&self) -> (u64, oneshot::Receiver<Answer>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        (id, rx)
    }

    /// Hand an answer to whoever is waiting on `id`. Returns false when nobody is: the
    /// request timed out, or the window answered twice. Both are the window's business
    /// to know about, and neither is an error here.
    pub fn finish(&self, id: u64, answer: Answer) -> bool {
        let Some(tx) = self.waiting.lock().unwrap().remove(&id) else {
            return false;
        };
        tx.send(answer).is_ok()
    }

    /// Give up on `id`. Called on timeout so a late answer has nowhere to pile up.
    pub fn cancel(&self, id: u64) {
        self.waiting.lock().unwrap().remove(&id);
    }

    pub fn timeout(&self) -> Duration {
        match self.timeout_ms.load(Ordering::Relaxed) {
            0 => TIMEOUT,
            ms => Duration::from_millis(ms),
        }
    }

    #[cfg(test)]
    pub fn set_timeout_ms(&self, ms: u64) {
        self.timeout_ms.store(ms, Ordering::Relaxed);
    }

    /// How many requests are still waiting. Only used by tests, to prove that a timeout
    /// leaves nothing behind.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.waiting.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_reaches_the_caller_that_asked_for_it() {
        let b = Bridge::default();
        let (one, rx1) = b.begin();
        let (two, rx2) = b.begin();
        assert_ne!(one, two, "two requests must not share a ticket");

        // Answered out of order, because the window has no reason to answer in order.
        assert!(b.finish(two, Ok(vec![2])));
        assert!(b.finish(one, Ok(vec![1])));
        assert_eq!(rx1.await.unwrap().unwrap(), vec![1]);
        assert_eq!(rx2.await.unwrap().unwrap(), vec![2]);
        assert_eq!(b.pending(), 0);
    }

    #[tokio::test]
    async fn a_failure_from_the_window_is_delivered_as_a_failure() {
        let b = Bridge::default();
        let (id, rx) = b.begin();
        assert!(b.finish(id, Err("no canvas".into())));
        assert_eq!(rx.await.unwrap().unwrap_err(), "no canvas");
    }

    #[tokio::test]
    async fn a_late_or_duplicate_answer_is_dropped_rather_than_kept() {
        let b = Bridge::default();
        let (id, rx) = b.begin();
        b.cancel(id);
        // Nobody is waiting any more, and the entry is gone, so a window that answers
        // after the caller gave up cannot grow this map.
        assert!(!b.finish(id, Ok(vec![1])));
        assert_eq!(b.pending(), 0);
        assert!(rx.await.is_err(), "the receiver must see the sender dropped, not a value");
        // An id that never existed is the same story.
        assert!(!b.finish(9999, Ok(vec![1])));
    }
}
