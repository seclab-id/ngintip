//! Message bodies: stream bytes through while keeping a bounded copy for History.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{ready, Context, Poll},
};

use bytes::{Bytes, BytesMut};
use http_body::{Body as HttpBody, Frame, SizeHint};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Empty, Full};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = UnsyncBoxBody<Bytes, BoxError>;

/// Read the whole body up front (for intercept/rules) only below this size;
/// larger bodies stream past instead.
pub const CAPTURE_LIMIT: usize = 8 * 1024 * 1024;

/// Bytes of each body kept in the project file. Bodies stream through in full;
/// this is only a safety ceiling so a multi-GB transfer cannot exhaust memory
/// before it is written. Anything under it is stored whole.
pub const STORE_LIMIT: usize = 256 * 1024 * 1024;

pub fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed_unsync()
}

pub fn empty() -> Body {
    Empty::new().map_err(|never| match never {}).boxed_unsync()
}

pub fn boxed<B>(body: B) -> Body
where
    B: HttpBody<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    body.map_err(Into::into).boxed_unsync()
}

/// The part of a body seen so far.
#[derive(Default)]
pub struct Capture {
    /// At most [`STORE_LIMIT`] bytes.
    pub data: BytesMut,
    /// Bytes that actually went through.
    pub total: u64,
    /// Set when the stream failed or was cut off before the end.
    pub error: Option<String>,
}

impl Capture {
    pub fn truncated(&self) -> bool {
        self.total > self.data.len() as u64
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len() as u64;
        let room = STORE_LIMIT.saturating_sub(self.data.len());
        self.data.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }
}

pub type SharedCapture = Arc<Mutex<Capture>>;

type OnDone = Box<dyn FnOnce() + Send>;

/// Passes `inner` through unchanged, copying it into `capture`. `on_done` runs
/// once: at the end of the stream, on error, or when dropped early.
pub struct Tee<B: HttpBody> {
    inner: B,
    capture: SharedCapture,
    on_done: Option<OnDone>,
}

impl<B: HttpBody> Tee<B> {
    pub fn new(inner: B, capture: SharedCapture, on_done: Option<OnDone>) -> Self {
        Self {
            inner,
            capture,
            on_done,
        }
    }

    fn finish(&mut self, error: Option<String>) {
        if let Some(error) = error {
            self.capture.lock().unwrap().error.get_or_insert(error);
        }
        if let Some(done) = self.on_done.take() {
            done();
        }
    }
}

impl<B> HttpBody for Tee<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        match ready!(Pin::new(&mut this.inner).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    this.capture.lock().unwrap().push(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(e)) => {
                let e: BoxError = e.into();
                this.finish(Some(e.to_string()));
                Poll::Ready(Some(Err(e)))
            }
            None => {
                this.finish(None);
                Poll::Ready(None)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<B: HttpBody> Drop for Tee<B> {
    fn drop(&mut self) {
        if self.on_done.is_some() {
            // Readers may stop polling once the last byte arrived; that's not a failure.
            let error = (!self.inner.is_end_stream()).then(|| "transfer interrupted before the body completed".to_string());
            self.finish(error);
        }
    }
}

/// `prefix` followed by whatever is left of `rest`.
pub struct Chain<B> {
    prefix: Option<Bytes>,
    rest: B,
}

impl<B> HttpBody for Chain<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        if let Some(prefix) = self.prefix.take() {
            return Poll::Ready(Some(Ok(Frame::data(prefix))));
        }
        Pin::new(&mut self.rest).poll_frame(cx).map_err(Into::into)
    }

    fn is_end_stream(&self) -> bool {
        self.prefix.is_none() && self.rest.is_end_stream()
    }
}

pub enum Buffered<B> {
    Complete(Bytes),
    /// Went over the limit: what was read so far plus the unread rest.
    TooLarge(Chain<B>),
}

/// Read the whole body if it fits in `limit` bytes.
pub async fn buffer<B>(mut body: B, limit: usize) -> Result<Buffered<B>, BoxError>
where
    B: HttpBody<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    if body.size_hint().lower() > limit as u64 {
        return Ok(Buffered::TooLarge(Chain { prefix: None, rest: body }));
    }
    let mut buf = BytesMut::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.map_err(Into::into)?.into_data() {
            buf.extend_from_slice(&data);
            if buf.len() > limit {
                return Ok(Buffered::TooLarge(Chain {
                    prefix: Some(buf.freeze()),
                    rest: body,
                }));
            }
        }
    }
    Ok(Buffered::Complete(buf.freeze()))
}

/// Read at most `limit` bytes; the flag says whether more was left unread.
pub async fn collect_limited<B>(mut body: B, limit: usize) -> Result<(Bytes, bool), BoxError>
where
    B: HttpBody<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    let mut buf = BytesMut::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.map_err(Into::into)?.into_data() {
            let room = limit - buf.len();
            if data.len() > room {
                buf.extend_from_slice(&data[..room]);
                return Ok((buf.freeze(), true));
            }
            buf.extend_from_slice(&data);
        }
    }
    Ok((buf.freeze(), false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::StreamBody;

    fn chunks(parts: &[&'static [u8]]) -> impl HttpBody<Data = Bytes, Error = BoxError> + Unpin {
        let frames: Vec<Result<Frame<Bytes>, BoxError>> =
            parts.iter().map(|p| Ok(Frame::data(Bytes::from_static(p)))).collect();
        StreamBody::new(futures_util::stream::iter(frames))
    }

    #[tokio::test]
    async fn tee_passes_everything_and_keeps_a_copy() {
        let capture = SharedCapture::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let tee = Tee::new(chunks(&[b"hello ", b"world"]), capture.clone(), Some(Box::new(move || tx.send(()).unwrap())));
        let out = tee.collect().await.unwrap().to_bytes();
        assert_eq!(&out[..], b"hello world");
        rx.try_recv().expect("on_done ran");
        let c = capture.lock().unwrap();
        assert_eq!((&c.data[..], c.total, c.truncated(), c.error.is_none()), (&b"hello world"[..], 11, false, true));
    }

    #[tokio::test]
    async fn dropping_early_reports_interruption() {
        let capture = SharedCapture::default();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut tee = Tee::new(chunks(&[b"a", b"b"]), capture.clone(), Some(Box::new(move || tx.send(()).unwrap())));
        tee.frame().await.unwrap().unwrap();
        drop(tee);
        rx.try_recv().expect("on_done ran on drop");
        assert!(capture.lock().unwrap().error.is_some());
    }

    #[tokio::test]
    async fn buffer_falls_back_to_streaming_when_too_large() {
        match buffer(chunks(&[b"abc", b"def"]), 10).await.unwrap() {
            Buffered::Complete(b) => assert_eq!(&b[..], b"abcdef"),
            Buffered::TooLarge(_) => panic!("fits"),
        }
        match buffer(chunks(&[b"abc", b"def", b"ghi"]), 4).await.unwrap() {
            Buffered::Complete(_) => panic!("too large"),
            Buffered::TooLarge(rest) => {
                assert_eq!(&rest.collect().await.unwrap().to_bytes()[..], b"abcdefghi", "nothing lost");
            }
        }
    }
}
