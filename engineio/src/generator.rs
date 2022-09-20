use std::pin::Pin;

use futures_util::{Stream, StreamExt};

/// A generator is an internal type that represents a [`Send`] [`futures_util::Stream`]
/// that yields a certain type `T` whenever it's polled.
pub type Generator<T> = Pin<Box<dyn Stream<Item = T> + 'static + Send>>;

/// An internal type that implements stream by repeatedly calling [`Stream::poll_next`] on an
/// underlying stream. Note that the generic parameter will be wrapped in a [`Result`].
// #[derive(Clone)]
pub struct StreamGenerator<T, E> {
    inner: Generator<Result<T, E>>,
}

impl<T, E> Stream for StreamGenerator<T, E> {
    type Item = Result<T, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.inner.poll_next_unpin(cx)
    }
}

impl<T, E> StreamGenerator<T, E> {
    pub fn new(generator_stream: Generator<Result<T, E>>) -> Self {
        StreamGenerator {
            inner: generator_stream,
        }
    }
}
