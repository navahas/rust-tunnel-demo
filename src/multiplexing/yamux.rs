use std::{future::poll_fn, io, marker::PhantomData, pin::Pin, task::Poll};

use futures::{channel::oneshot, task::Context, Stream};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::mpsc,
};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use tracing::{debug, error, warn};
// Reexport
use yamux::Mode;

use crate::{
    multiplexing::YamuxControlError,
    utils::{
        atomic_ref_counter::{AtomicRefCounter, AtomicRefCounterGuard},
        stream_id,
        stream_id::StreamId,
    },
};

const LOG_TARGET: &str = "comms::multiplexing::yamux";

pub struct Yamux {
    control: Control,
    incoming: IncomingSubstreams,
    substream_counter: AtomicRefCounter,
}

impl Yamux {
    /// Upgrade the underlying socket to use yamux
    pub fn upgrade_connection<TSocket>(
        socket: TSocket,
        direction: crate::multiplexing::direction::ConnectionDirection,
    ) -> io::Result<Self>
    where
        TSocket: AsyncRead + AsyncWrite + Send + Sync + Unpin + 'static,
    {
        let mode = match direction {
            crate::multiplexing::direction::ConnectionDirection::Inbound => Mode::Server,
            crate::multiplexing::direction::ConnectionDirection::Outbound => Mode::Client,
        };

        let config = yamux::Config::default();

        let substream_counter = AtomicRefCounter::new();
        let connection = yamux::Connection::new(socket.compat(), config, mode);
        let (control, incoming) =
            Self::spawn_incoming_stream_worker(connection, substream_counter.clone());

        Ok(Self {
            control,
            incoming,
            substream_counter,
        })
    }

    // yamux@0.4 requires the incoming substream stream be polled in order to make progress on requests from it's
    // Control api. Here we spawn off a worker which will do this job
    fn spawn_incoming_stream_worker<TSocket>(
        connection: yamux::Connection<TSocket>,
        counter: AtomicRefCounter,
    ) -> (Control, IncomingSubstreams)
    where
        TSocket: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + Sync + 'static,
    {
        let (incoming_tx, incoming_rx) = mpsc::channel(10);
        let (request_tx, request_rx) = mpsc::channel(1);
        let incoming = YamuxWorker::new(incoming_tx, request_rx, counter.clone());
        let control = Control::new(request_tx);
        tokio::spawn(incoming.run(connection));
        (control, IncomingSubstreams::new(incoming_rx, counter))
    }

    /// Get the yamux control struct
    pub fn get_yamux_control(&self) -> Control {
        self.control.clone()
    }

    /// Returns a mutable reference to a `Stream` that emits substreams initiated by the remote
    pub fn incoming_mut(&mut self) -> &mut IncomingSubstreams {
        &mut self.incoming
    }

    /// Consumes this object and returns a `Stream` that emits substreams initiated by the remote
    pub fn into_incoming(self) -> IncomingSubstreams {
        self.incoming
    }

    /// Return the number of active substreams
    pub fn substream_count(&self) -> usize {
        self.substream_counter.get()
    }

    /// Return a SubstreamCounter for this connection
    pub(crate) fn substream_counter(&self) -> AtomicRefCounter {
        self.substream_counter.clone()
    }
}

#[derive(Debug)]
pub enum YamuxRequest {
    OpenStream {
        reply: oneshot::Sender<yamux::Result<Substream>>,
    },
    Close {
        reply: oneshot::Sender<yamux::Result<()>>,
    },
}

#[derive(Clone)]
pub struct Control {
    request_tx: mpsc::Sender<YamuxRequest>,
}

impl Control {
    pub fn new(request_tx: mpsc::Sender<YamuxRequest>) -> Self {
        Self { request_tx }
    }

    /// Open a new stream to the remote.
    pub async fn open_stream(&mut self) -> Result<Substream, YamuxControlError> {
        let (reply, reply_rx) = oneshot::channel();
        self.request_tx
            .send(YamuxRequest::OpenStream { reply })
            .await?;
        let stream = reply_rx.await??;
        Ok(stream)
    }

    /// Close the connection.
    pub async fn close(&mut self) -> Result<(), YamuxControlError> {
        let (reply, reply_rx) = oneshot::channel();
        self.request_tx.send(YamuxRequest::Close { reply }).await?;
        Ok(reply_rx.await??)
    }
}

pub struct IncomingSubstreams {
    inner: mpsc::Receiver<yamux::Stream>,
    substream_counter: AtomicRefCounter,
}

impl IncomingSubstreams {
    pub(self) fn new(
        inner: mpsc::Receiver<yamux::Stream>,
        substream_counter: AtomicRefCounter,
    ) -> Self {
        Self {
            inner,
            substream_counter,
        }
    }

    pub fn substream_count(&self) -> usize {
        self.substream_counter.get()
    }
}

impl Stream for IncomingSubstreams {
    type Item = Substream;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match futures::ready!(Pin::new(&mut self.inner).poll_recv(cx)) {
            Some(stream) => Poll::Ready(Some(Substream {
                stream: stream.compat(),
                _counter_guard: self.substream_counter.new_guard(),
            })),
            None => Poll::Ready(None),
        }
    }
}

/// A yamux stream wrapper that can be read from and written to.
#[derive(Debug)]
pub struct Substream {
    stream: Compat<yamux::Stream>,
    _counter_guard: AtomicRefCounterGuard,
}

impl StreamId for Substream {
    fn stream_id(&self) -> stream_id::Id {
        self.stream.get_ref().id().into()
    }
}

impl tokio::io::AsyncRead for Substream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.stream).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                #[cfg(feature = "metrics")]
                super::metrics::TOTAL_BYTES_READ.inc_by(buf.filled().len() as u64);
                Poll::Ready(Ok(()))
            }
            res => res,
        }
    }
}

impl tokio::io::AsyncWrite for Substream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        #[cfg(feature = "metrics")]
        super::metrics::TOTAL_BYTES_WRITTEN.inc_by(buf.len() as u64);
        Pin::new(&mut self.stream).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl From<yamux::StreamId> for stream_id::Id {
    fn from(id: yamux::StreamId) -> Self {
        stream_id::Id::new(id.val())
    }
}

struct YamuxWorker<TSocket> {
    incoming_substreams: mpsc::Sender<yamux::Stream>,
    request_rx: mpsc::Receiver<YamuxRequest>,
    counter: AtomicRefCounter,
    _phantom: PhantomData<TSocket>,
}
    
impl<TSocket> YamuxWorker<TSocket>
where
    TSocket: futures::AsyncRead + futures::AsyncWrite + Unpin + Send + Sync + 'static,
{
    pub fn new(
        incoming_substreams: mpsc::Sender<yamux::Stream>,
        request_rx: mpsc::Receiver<YamuxRequest>,
        counter: AtomicRefCounter,
    ) -> Self {
        Self {
            incoming_substreams,
            request_rx,
            counter,
            _phantom: PhantomData,
        }
    }

    async fn run(mut self, mut connection: yamux::Connection<TSocket>) {
        loop {
            tokio::select! {
                biased;

                _ = self.incoming_substreams.closed() => {
                    debug!(
                        target: LOG_TARGET,
                        "{} Incoming peer substream task is stopping because the internal stream sender channel was \
                         closed",
                        self.counter.get()
                    );
                    // Ignore: we already log the error variant in Self::close
                    let _ignore = Self::close(&mut connection).await;
                    break
                },

                Some(request) = self.request_rx.recv() => {
                    if let Err(err) = self.handle_request(&mut connection, request).await {
                        error!(target: LOG_TARGET, "Error handling request: {err}");
                        break;
                    }
                },

                result = Self::next_inbound_stream(&mut connection) => {
                     match result {
                        Some(Ok(stream)) => {
                            if self.incoming_substreams.send(stream).await.is_err() {
                                debug!(
                                    target: LOG_TARGET,
                                    "{} Incoming peer substream task is stopping because the internal stream sender channel was closed",
                                    self.counter.get()
                                );
                                break;
                            }
                        },
                        None =>{
                            debug!(
                                target: LOG_TARGET,
                                "{} Incoming peer substream ended.",
                                self.counter.get()
                            );
                            break;
                        }
                        Some(Err(err)) => {
                            error!(
                                target: LOG_TARGET,
                                "{} Incoming peer substream task received an error because '{}'",
                                self.counter.get(),
                                err
                            );
                            break;
                        },
                    }
                }
            }
        }
    }

    async fn handle_request(
        &self,
        connection_mut: &mut yamux::Connection<TSocket>,
        request: YamuxRequest,
    ) -> io::Result<()> {
        match request {
            YamuxRequest::OpenStream { reply } => {
                let result = poll_fn(move |cx| connection_mut.poll_new_outbound(cx)).await;
                if reply
                    .send(result.map(|stream| Substream {
                        stream: stream.compat(),
                        _counter_guard: self.counter.new_guard(),
                    }))
                    .is_err()
                {
                    warn!(target: LOG_TARGET, "Request to open substream was aborted before reply was sent");
                }
            }
            YamuxRequest::Close { reply } => {
                if reply.send(Self::close(connection_mut).await).is_err() {
                    warn!(target: LOG_TARGET, "Request to close substream was aborted before reply was sent");
                }
            }
        }
        Ok(())
    }

    async fn next_inbound_stream(
        connection_mut: &mut yamux::Connection<TSocket>,
    ) -> Option<yamux::Result<yamux::Stream>> {
        poll_fn(|cx| connection_mut.poll_next_inbound(cx)).await
    }

    async fn close(connection: &mut yamux::Connection<TSocket>) -> yamux::Result<()> {
        if let Err(err) = poll_fn(|cx| connection.poll_close(cx)).await {
            error!(target: LOG_TARGET, "Error while closing yamux connection: {}", err);
            return Err(err);
        }
        debug!(target: LOG_TARGET, "Yamux connection has closed");
        Ok(())
    }
}
