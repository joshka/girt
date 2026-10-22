//! Runs a synchronous serving session on a blocking executor over async streams.
//!
//! The session keeps its blocking `Read` and `Write` core. Two bounded channels carry bytes
//! between it and the caller's `AsyncRead` and `AsyncWrite`, so at most [`CHANNEL_CHUNKS`] chunks
//! of [`CHUNK_BYTES`] wait in each direction and a slow client stalls pack generation instead of
//! growing a buffer.

use std::any::Any;
use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use super::ServeError;

/// The most bytes one channel message carries.
const CHUNK_BYTES: usize = 64 * 1024;

/// The most messages waiting in each direction.
const CHANNEL_CHUNKS: usize = 4;

/// Runs blocking serving work away from the async runtime's I/O threads.
///
/// Serving is CPU-heavy: before the first pack byte, a fetch counts every object it will send,
/// and then it compresses or copies them. Running that on an async worker thread would stall
/// every other connection on that thread, so [`run_blocking`] hands it to an executor.
///
/// [`SpawnBlocking`] uses Tokio's blocking pool, and a [`tokio::runtime::Handle`] uses another
/// runtime's pool. Implement this trait to use a dedicated thread pool, for example to cap how
/// many packs are generated at once.
///
/// # Examples
///
/// Run each session on its own thread:
///
/// ```
/// use girt::serve::BlockingExecutor;
///
/// struct ThreadPerSession;
///
/// impl BlockingExecutor for ThreadPerSession {
///     fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
///         std::thread::spawn(job);
///     }
/// }
/// ```
pub trait BlockingExecutor {
    /// Runs `job` to completion on a thread that may block.
    ///
    /// The job must run eventually. If the executor drops it instead, the session fails with
    /// [`ServeError::Executor`].
    fn spawn(&self, job: Box<dyn FnOnce() + Send>);
}

/// Runs jobs on the current Tokio runtime's blocking pool, through
/// [`tokio::task::spawn_blocking`].
///
/// # Panics
///
/// [`run_blocking`] panics if it isn't polled inside a Tokio runtime.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpawnBlocking;

impl BlockingExecutor for SpawnBlocking {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        drop(tokio::task::spawn_blocking(job));
    }
}

impl BlockingExecutor for tokio::runtime::Handle {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        drop(self.spawn_blocking(job));
    }
}

impl<E: BlockingExecutor + ?Sized> BlockingExecutor for &E {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        (**self).spawn(job);
    }
}

impl<E: BlockingExecutor + ?Sized> BlockingExecutor for Arc<E> {
    fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
        (**self).spawn(job);
    }
}

/// Runs a synchronous serving session on `executor`, reading the client's bytes from `input`
/// and writing the response to `output`.
///
/// `session` receives blocking ends of both streams and the cancellation flag, and calls any
/// synchronous serving API with them, such as [`UploadPack::serve`](super::UploadPack::serve) or
/// [`ReceivePack::serve`](super::ReceivePack::serve). It opens the repository itself, so nothing
/// it borrows has to outlive the future.
///
/// Bytes cross between the streams and the session in chunks of up to 64 KiB, with at most four
/// chunks waiting in each direction. When the client reads slowly, the session blocks on its
/// next write rather than buffering the pack. Output is passed on when the session flushes, when
/// a chunk fills, before the session waits for input, and when it returns.
///
/// Dropping the returned future sets the cancellation flag, which the session checks between
/// packets, object reads and pack writes. Failing to write `output` sets it too. The executor's
/// thread stays busy until the session notices.
///
/// # Errors
///
/// Returns the session's error, or [`ServeError::Io`] when writing `output` failed, or
/// [`ServeError::Executor`] when the executor dropped the job without running it.
///
/// # Panics
///
/// Resumes the session's panic.
///
/// # Examples
///
/// Serve a full-duplex fetch session, as over SSH, with streams that implement `AsyncRead` and
/// `AsyncWrite`:
///
/// ```
/// use std::path::PathBuf;
///
/// use girt::Repository;
/// use girt::serve::{Protocol, ServeError, SpawnBlocking, UploadPack, run_blocking};
/// use tokio::io::{AsyncRead, AsyncWrite};
///
/// async fn serve_fetch(
///     path: PathBuf,
///     input: impl AsyncRead + Unpin,
///     output: impl AsyncWrite + Unpin,
/// ) -> Result<(), ServeError> {
///     run_blocking(
///         SpawnBlocking,
///         input,
///         output,
///         move |input, output, cancel| {
///             let repository =
///                 Repository::open(&path).map_err(|error| ServeError::Open(Box::new(error)))?;
///             UploadPack::new(&repository)?.serve(Protocol::V2, input, output, cancel)
///         },
///     )
///     .await
/// }
/// ```
pub async fn run_blocking<T, F>(
    executor: impl BlockingExecutor,
    mut input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    session: F,
) -> Result<T, ServeError>
where
    T: Send + 'static,
    F: FnOnce(&mut ChannelReader, &mut ChannelWriter, &AtomicBool) -> Result<T, ServeError>
        + Send
        + 'static,
{
    let cancel = CancelOnDrop(Arc::new(AtomicBool::new(false)));
    let (input_sender, input_receiver) = mpsc::channel::<io::Result<Vec<u8>>>(CHANNEL_CHUNKS);
    let (output_sender, mut output_receiver) = mpsc::channel::<Vec<u8>>(CHANNEL_CHUNKS);
    let (result_sender, result_receiver) = oneshot::channel::<JobResult<T>>();

    let flag = cancel.0.clone();
    executor.spawn(Box::new(move || {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let shared = Rc::new(RefCell::new(Pending {
                bytes: Vec::new(),
                sender: Some(output_sender),
            }));
            let mut reader = ChannelReader {
                receiver: input_receiver,
                chunk: Vec::new(),
                position: 0,
                output: shared.clone(),
            };
            let mut writer = ChannelWriter { pending: shared };
            let result = session(&mut reader, &mut writer, &flag);
            let flushed = writer.pending.borrow_mut().send();
            match (result, flushed) {
                (Ok(value), Err(error)) => {
                    drop(value);
                    Err(error.into())
                }
                (result, _) => result,
            }
        }));
        // The caller may already be gone.
        let _ = result_sender.send(result);
    }));

    // The input pump owns the sender, so dropping the pump ends the session's input.
    let pump_input = async move {
        let mut buffer = vec![0; CHUNK_BYTES];
        loop {
            let message = match input.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => Ok(buffer.get(..read).unwrap_or_default().to_vec()),
                Err(error) => Err(error),
            };
            let failed = message.is_err();
            if input_sender.send(message).await.is_err() || failed {
                break;
            }
        }
        // End of input closes the channel; the session reads it as end of stream.
        drop(input_sender);
        std::future::pending::<()>().await;
    };
    // The output pump owns the receiver, so dropping the pump fails the session's next send.
    let pump_output = async move {
        while let Some(chunk) = output_receiver.recv().await {
            output.write_all(&chunk).await?;
            // Pass each flushed chunk on promptly: a client may be waiting for it to reply.
            if output_receiver.is_empty() {
                output.flush().await?;
            }
        }
        output.flush().await
    };
    // The output pump ends once the session drops its sender, or on the first failed write.
    let written = tokio::select! {
        biased;
        written = pump_output => written,
        () = pump_input => unreachable_pending(),
    };
    // Both pumps are gone now. After a failed write, the cancel flag, the closed output channel
    // and the ended input wake the session wherever it is blocked.
    if written.is_err() {
        cancel.0.store(true, Ordering::Relaxed);
    }
    let result = match result_receiver.await {
        Ok(Ok(result)) => result,
        Ok(Err(panic)) => resume_unwind(panic),
        Err(_) => Err(ServeError::Executor),
    };
    match written {
        // A failed write stops the session; report the write rather than how the session ended.
        Err(error) => Err(ServeError::Io(error)),
        Ok(()) => result,
    }
}

type JobResult<T> = Result<Result<T, ServeError>, Box<dyn Any + Send>>;

#[expect(
    clippy::unreachable,
    reason = "the input pump never completes; it waits once input ends"
)]
fn unreachable_pending<T>() -> T {
    unreachable!("the input pump never completes")
}

/// Sets the cancellation flag when the future that owns it is dropped or finishes.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Output the session wrote but hasn't passed on yet.
struct Pending {
    bytes: Vec<u8>,
    /// `None` once the async side stopped receiving.
    sender: Option<mpsc::Sender<Vec<u8>>>,
}

impl Pending {
    fn send(&mut self) -> io::Result<()> {
        if self.bytes.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::take(&mut self.bytes);
        let sent = match &self.sender {
            Some(sender) => sender.blocking_send(bytes).is_ok(),
            None => false,
        };
        if sent {
            Ok(())
        } else {
            self.sender = None;
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the response stream closed",
            ))
        }
    }
}

/// The blocking end of [`run_blocking`]'s input: reads the bytes the client sent.
///
/// Before it waits for input, it passes on any output the session has written, so a reply the
/// session didn't flush can't leave both sides waiting.
pub struct ChannelReader {
    receiver: mpsc::Receiver<io::Result<Vec<u8>>>,
    chunk: Vec<u8>,
    position: usize,
    output: Rc<RefCell<Pending>>,
}

impl std::fmt::Debug for ChannelReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelReader")
            .field("buffered", &(self.chunk.len() - self.position))
            .finish_non_exhaustive()
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.position == self.chunk.len() {
            self.output.borrow_mut().send()?;
            match self.receiver.blocking_recv() {
                None => return Ok(0),
                Some(chunk) => {
                    self.chunk = chunk?;
                    self.position = 0;
                }
            }
        }
        let available = self.chunk.get(self.position..).unwrap_or_default();
        let count = available.len().min(buffer.len());
        if let (Some(target), Some(source)) = (buffer.get_mut(..count), available.get(..count)) {
            target.copy_from_slice(source);
        }
        self.position += count;
        Ok(count)
    }
}

/// The blocking end of [`run_blocking`]'s output: writes the response to the client.
///
/// Writes collect into chunks of up to 64 KiB. A chunk is passed on when it fills or the session
/// flushes, and blocks while the async side is still writing earlier chunks.
pub struct ChannelWriter {
    pending: Rc<RefCell<Pending>>,
}

impl std::fmt::Debug for ChannelWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelWriter")
            .field("buffered", &self.pending.borrow().bytes.len())
            .finish_non_exhaustive()
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut pending = self.pending.borrow_mut();
        if pending.sender.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the response stream closed",
            ));
        }
        let room = CHUNK_BYTES - pending.bytes.len();
        let count = room.min(bytes.len());
        pending
            .bytes
            .extend_from_slice(bytes.get(..count).unwrap_or_default());
        if pending.bytes.len() == CHUNK_BYTES {
            pending.send()?;
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.pending.borrow_mut().send()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn round_trips_input_to_output() {
        let input = vec![7u8; 300_000];
        let mut output = Vec::new();
        let count = run_blocking(
            SpawnBlocking,
            &input[..],
            &mut output,
            |reader, writer, _| {
                let mut bytes = Vec::new();
                reader.read_to_end(&mut bytes)?;
                writer.write_all(&bytes)?;
                Ok(bytes.len())
            },
        )
        .await
        .unwrap();
        assert_eq!(count, input.len());
        assert_eq!(output, input);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slow_reader_bounds_buffered_output() {
        let written = Arc::new(AtomicUsize::new(0));
        let counter = written.clone();
        let total = 64 * CHUNK_BYTES;
        let (client, server) = tokio::io::duplex(CHUNK_BYTES);
        let serve = tokio::spawn(run_blocking(
            SpawnBlocking,
            tokio::io::empty(),
            server,
            move |_, writer, _| {
                let chunk = vec![1u8; 4096];
                while counter.load(Ordering::Relaxed) < total {
                    writer.write_all(&chunk)?;
                    counter.fetch_add(chunk.len(), Ordering::Relaxed);
                }
                Ok(())
            },
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        // The channel, the chunk being written, the duplex buffer and the session's own chunk.
        let bound = (CHANNEL_CHUNKS + 3) * CHUNK_BYTES;
        let stalled = written.load(Ordering::Relaxed);
        assert!(stalled <= bound, "{stalled} bytes written without a reader");
        let mut client = client;
        let mut received = Vec::new();
        client.read_to_end(&mut received).await.unwrap();
        serve.await.unwrap().unwrap();
        assert_eq!(received.len(), total);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_future_cancels_the_session() {
        let (stopped_sender, stopped) = std::sync::mpsc::channel();
        let (started_sender, started) = oneshot::channel();
        let serve = run_blocking(
            SpawnBlocking,
            tokio::io::empty(),
            tokio::io::sink(),
            move |_, _, cancel| {
                let _ = started_sender.send(());
                while !cancel.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let _ = stopped_sender.send(());
                Err::<(), _>(ServeError::Cancelled)
            },
        );
        let serve = tokio::spawn(serve);
        started.await.unwrap();
        serve.abort();
        let stopped =
            tokio::task::spawn_blocking(move || stopped.recv_timeout(Duration::from_secs(10)));
        assert!(stopped.await.unwrap().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn closed_output_cancels_the_session() {
        let (client, server) = tokio::io::duplex(1024);
        drop(client);
        let result = run_blocking(
            SpawnBlocking,
            tokio::io::empty(),
            server,
            |_, writer, cancel| {
                let chunk = vec![1u8; CHUNK_BYTES];
                while !cancel.load(Ordering::Relaxed) {
                    writer.write_all(&chunk)?;
                }
                Err::<(), _>(ServeError::Cancelled)
            },
        )
        .await;
        assert!(matches!(result, Err(ServeError::Io(_))), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_job_reports_the_executor() {
        struct Drops;
        impl BlockingExecutor for Drops {
            fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
                drop(job);
            }
        }
        let result = run_blocking(Drops, tokio::io::empty(), tokio::io::sink(), |_, _, _| {
            Ok(())
        })
        .await;
        assert!(matches!(result, Err(ServeError::Executor)), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_write_wakes_a_session_waiting_for_input() {
        let (client, server) = tokio::io::duplex(1024);
        drop(client);
        // The client never sends or closes its input.
        let (_input_client, input) = tokio::io::duplex(1024);
        let serving = run_blocking(SpawnBlocking, input, server, |reader, writer, _| {
            writer.write_all(b"advertisement")?;
            writer.flush()?;
            let mut byte = [0];
            reader.read_exact(&mut byte)?;
            Ok(())
        });
        let result = tokio::time::timeout(Duration::from_secs(10), serving)
            .await
            .expect("the session stayed blocked on input");
        assert!(matches!(result, Err(ServeError::Io(_))), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_flush_stops_a_session_that_keeps_writing() {
        struct FailsFlush;
        impl AsyncWrite for FailsFlush {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                bytes: &[u8],
            ) -> std::task::Poll<io::Result<usize>> {
                std::task::Poll::Ready(Ok(bytes.len()))
            }
            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                std::task::Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
            }
            fn poll_shutdown(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                std::task::Poll::Ready(Ok(()))
            }
        }
        let serving = run_blocking(
            SpawnBlocking,
            tokio::io::empty(),
            FailsFlush,
            |_, writer, _| {
                let chunk = vec![1u8; CHUNK_BYTES];
                loop {
                    writer.write_all(&chunk)?;
                }
            },
        );
        let result: Result<(), _> = tokio::time::timeout(Duration::from_secs(10), serving)
            .await
            .expect("the session stayed blocked on output");
        assert!(matches!(result, Err(ServeError::Io(_))), "{result:?}");
    }
}
