//! Object storage that algorithms can read from without knowing where objects live.
//!
//! [`ObjectRead`] and [`ObjectWrite`] describe a content-addressed Git object store with an async
//! boundary: a read may wait on a disk, a database or a network. [`Objects`] implements
//! [`ObjectRead`] over a repository's `.git/objects` directory, and [`MemoryObjects`] implements
//! both over a map in memory. An algorithm written against `S: ObjectRead + ?Sized` runs on either,
//! and on `dyn DynObjectRead`, such as [`crate::compare_trees_in`].
//!
//! Reference storage is separate and not yet pluggable; see
//! [issue 515](https://github.com/joshka/girt/issues/515) for the design.
//!
//! # Async boundary, synchronous work
//!
//! Only loading and waiting are async. Inflating, applying deltas, hashing and parsing are linear
//! work over bytes already in memory, so they stay ordinary functions that a store calls. The
//! question at the boundary is where that work runs. The filesystem store runs a read inline when
//! the stored header says the payload is at most [`INLINE_PAYLOAD_BYTES`]; measured reads of that
//! size take under about 100 µs, comparable to handing the work to another thread. With the
//! `tokio` feature and a current Tokio runtime, larger payloads run under
//! `tokio::task::spawn_blocking`. [`block_on`] gives synchronous callers the same operations.
//!
//! Dropping a future or stream cancels it. A read already moved to a blocking thread is told to
//! stop at its next checkpoint.
//!
//! # Verified results
//!
//! A read returns an [`Object`], whose payload is shared with the store's cache rather than copied
//! into a caller buffer. A store outside girt builds one with [`Object::verified`], which hashes
//! the payload and refuses it unless it matches the requested identity. A backend therefore cannot
//! hand an algorithm content under the wrong name by asserting an identity.
//!
//! # Dynamic dispatch
//!
//! [`ObjectRead`] uses `async fn` in traits, so generic callers get unboxed futures and the
//! filesystem path allocates nothing extra. Those methods cannot be called through `dyn`;
//! [`DynObjectRead`] is the object-safe twin, implemented for every store, and boxes one future per
//! call. `dyn DynObjectRead` implements [`ObjectRead`], so generic algorithms accept it.
//!
//! ```
//! use girt::storage::{MemoryObjects, ObjectRead, ObjectWrite, block_on};
//! use girt::{ObjectFormat, ObjectKind, ReadLimits};
//!
//! let store = MemoryObjects::new(ObjectFormat::Sha1);
//! let id = block_on(store.write(ObjectKind::Blob, b"notes\n"))?;
//!
//! let object = block_on(store.read(id, ReadLimits::default()))?;
//! assert_eq!(object.ok_or("absent")?.data(), b"notes\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, PoisonError, RwLock};
use std::task::{Context, Poll, Wake, Waker};

pub use futures_core::Stream;

use crate::{
    Object, ObjectFormat, ObjectHeader, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits,
};

/// A boxed future, as returned through [`DynObjectRead`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed stream, as returned through [`DynObjectRead`].
pub type BoxStream<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + 'a>>;

/// One result of [`ObjectRead::read_many`]: the identity and the object, or `None` when absent.
pub type ReadManyItem = Result<(ObjectId, Option<Object>), StoreError>;

/// Largest payload the filesystem store decodes inline on an async task.
///
/// Measured warm reads of 64 KiB took 30–95 µs and 1 MiB took 0.4–0.85 ms, against about 8 µs
/// for an empty `spawn_blocking` round trip; see issue 515 for the table.
pub const INLINE_PAYLOAD_BYTES: u64 = 64 * 1024;

/// Reads Git objects by identity from some store.
///
/// Absence is `Ok(None)` or `Ok(false)`, never an error. It means absent from this store's
/// current view, not absent everywhere: a store does not fetch. Futures are lazy and do nothing
/// until polled; dropping one cancels it.
pub trait ObjectRead: Send + Sync {
    /// The format of every identity this store holds.
    fn object_format(&self) -> ObjectFormat;

    /// Reads and verifies the object `id`, or returns `None` when the store does not hold it.
    ///
    /// `limits` bounds the decoded payload and decoding work. The returned object hashes to `id`.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] for a foreign-format `id`, damaged content, an exhausted limit or
    /// a backend failure.
    fn read(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> impl Future<Output = Result<Option<Object>, StoreError>> + Send;

    /// Reads the kind and size the store records for `id`, without verifying the payload.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read`].
    fn header(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> impl Future<Output = Result<Option<ObjectHeader>, StoreError>> + Send;

    /// Reports whether the store holds `id`, without reading or verifying it.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] for a foreign-format `id` or a backend failure.
    fn contains(&self, id: ObjectId) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// Reads each of `ids` and yields one result per identity, in the order given.
    ///
    /// The default reads one at a time through [`Self::read`]. A store that can answer many
    /// identities in one request, such as a remote database, should override it.
    fn read_many<'a>(
        &'a self,
        ids: &'a [ObjectId],
        limits: ReadLimits,
    ) -> impl Stream<Item = ReadManyItem> + Send + 'a {
        ReadEach {
            ids: ids.iter(),
            pending: None,
            read: move |id| Box::pin(self.read(id, limits)) as BoxFuture<'a, _>,
        }
    }
}

/// Stores Git objects, computing their identities.
pub trait ObjectWrite: ObjectRead {
    /// Stores `data` as an object of `kind` and returns its identity.
    ///
    /// The store hashes the payload itself. Writing identical content again is allowed and
    /// returns the same identity; the store decides whether to keep a second copy. After success,
    /// [`ObjectRead::read`] on the same store returns the object.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the backend cannot store the object.
    fn write(
        &self,
        kind: ObjectKind,
        data: &[u8],
    ) -> impl Future<Output = Result<ObjectId, StoreError>> + Send;
}

/// The object-safe form of [`ObjectRead`], for `&dyn DynObjectRead` and `Box<dyn DynObjectRead>`.
///
/// Every [`ObjectRead`] store implements it, and `dyn DynObjectRead` implements [`ObjectRead`].
/// Each call boxes its future. The method names differ from [`ObjectRead`]'s so that both traits
/// can be in scope.
///
/// ```
/// use girt::storage::{DynObjectRead, MemoryObjects, ObjectWrite, block_on};
/// use girt::{ObjectFormat, ObjectKind};
///
/// let memory = MemoryObjects::new(ObjectFormat::Sha1);
/// let id = block_on(memory.write(ObjectKind::Blob, b"x"))?;
///
/// let store: Box<dyn DynObjectRead> = Box::new(memory);
/// assert!(block_on(store.contains_dyn(id))?);
/// # Ok::<(), girt::storage::StoreError>(())
/// ```
pub trait DynObjectRead: Send + Sync {
    /// [`ObjectRead::object_format`].
    fn object_format_dyn(&self) -> ObjectFormat;

    /// [`ObjectRead::read`].
    fn read_dyn(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> BoxFuture<'_, Result<Option<Object>, StoreError>>;

    /// [`ObjectRead::header`].
    fn header_dyn(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> BoxFuture<'_, Result<Option<ObjectHeader>, StoreError>>;

    /// [`ObjectRead::contains`].
    fn contains_dyn(&self, id: ObjectId) -> BoxFuture<'_, Result<bool, StoreError>>;

    /// [`ObjectRead::read_many`].
    fn read_many_dyn<'a>(
        &'a self,
        ids: &'a [ObjectId],
        limits: ReadLimits,
    ) -> BoxStream<'a, ReadManyItem>;
}

impl<S: ObjectRead> DynObjectRead for S {
    fn object_format_dyn(&self) -> ObjectFormat {
        self.object_format()
    }

    fn read_dyn(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> BoxFuture<'_, Result<Option<Object>, StoreError>> {
        Box::pin(self.read(id, limits))
    }

    fn header_dyn(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> BoxFuture<'_, Result<Option<ObjectHeader>, StoreError>> {
        Box::pin(self.header(id, limits))
    }

    fn contains_dyn(&self, id: ObjectId) -> BoxFuture<'_, Result<bool, StoreError>> {
        Box::pin(self.contains(id))
    }

    fn read_many_dyn<'a>(
        &'a self,
        ids: &'a [ObjectId],
        limits: ReadLimits,
    ) -> BoxStream<'a, ReadManyItem> {
        Box::pin(self.read_many(ids, limits))
    }
}

impl ObjectRead for dyn DynObjectRead + '_ {
    fn object_format(&self) -> ObjectFormat {
        self.object_format_dyn()
    }

    fn read(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> impl Future<Output = Result<Option<Object>, StoreError>> + Send {
        self.read_dyn(id, limits)
    }

    fn header(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> impl Future<Output = Result<Option<ObjectHeader>, StoreError>> + Send {
        self.header_dyn(id, limits)
    }

    fn contains(&self, id: ObjectId) -> impl Future<Output = Result<bool, StoreError>> + Send {
        self.contains_dyn(id)
    }

    fn read_many<'a>(
        &'a self,
        ids: &'a [ObjectId],
        limits: ReadLimits,
    ) -> impl Stream<Item = ReadManyItem> + Send + 'a {
        self.read_many_dyn(ids, limits)
    }
}

type ReadFuture<'a> = BoxFuture<'a, Result<Option<Object>, StoreError>>;

/// The default [`ObjectRead::read_many`]: one boxed read at a time, in order.
struct ReadEach<'a, F> {
    ids: std::slice::Iter<'a, ObjectId>,
    pending: Option<(ObjectId, ReadFuture<'a>)>,
    read: F,
}

impl<'a, F> Stream for ReadEach<'a, F>
where
    F: FnMut(ObjectId) -> ReadFuture<'a> + Unpin,
{
    type Item = ReadManyItem;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ReadManyItem>> {
        let this = self.get_mut();
        loop {
            if let Some((id, future)) = &mut this.pending {
                let id = *id;
                let result = std::task::ready!(future.as_mut().poll(cx));
                this.pending = None;
                return Poll::Ready(Some(result.map(|object| (id, object))));
            }
            let Some(&id) = this.ids.next() else {
                return Poll::Ready(None);
            };
            this.pending = Some((id, (this.read)(id)));
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let pending = usize::from(self.pending.is_some());
        let remaining = self.ids.len() + pending;
        (remaining, Some(remaining))
    }
}

/// Runs `future` to completion on the current thread, for synchronous callers.
///
/// The filesystem and memory stores finish in one poll when no Tokio runtime is current, so
/// this adds no thread hop or allocation on their hot path. Inside a Tokio runtime this blocks the
/// calling thread; call it from a blocking thread there, not a worker.
///
/// ```
/// use girt::storage::block_on;
/// assert_eq!(block_on(async { 1 + 1 }), 2);
/// ```
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    if let Poll::Ready(output) = future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        return output;
    }

    // The first poll may have registered the no-op waker; every later poll registers this one.
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::park();
    }
}

/// What kind of failure a [`StoreError`] reports, independent of the backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum StoreErrorKind {
    /// The request is invalid for this store, such as an identity in another format.
    InvalidInput,
    /// Stored content is damaged or does not match its identity.
    Corrupt,
    /// A read limit was exhausted.
    Limit,
    /// The caller cancelled the operation.
    Cancelled,
    /// The store does not support the requested content or operation.
    Unsupported,
    /// A concurrent writer conflicted with this operation.
    Conflict,
    /// The backend could not be reached or failed for reasons outside the request.
    Unavailable,
}

impl StoreErrorKind {
    fn describe(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid object store request",
            Self::Corrupt => "corrupt object store content",
            Self::Limit => "object store limit exceeded",
            Self::Cancelled => "object store operation cancelled",
            Self::Unsupported => "unsupported by object store",
            Self::Conflict => "object store conflict",
            Self::Unavailable => "object store unavailable",
        }
    }
}

/// A failure from an [`ObjectRead`] or [`ObjectWrite`] store.
///
/// [`Self::kind`] classifies the failure for callers. The backend's own error, if any, is the
/// [`std::error::Error::source`] and can be recovered with [`Self::downcast`].
pub struct StoreError {
    kind: StoreErrorKind,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl StoreError {
    /// Creates an error of `kind` caused by a backend-specific `source`.
    pub fn new(kind: StoreErrorKind, source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        Self {
            kind,
            source: Some(source.into()),
        }
    }

    /// The backend-independent classification.
    pub fn kind(&self) -> StoreErrorKind {
        self.kind
    }

    /// Returns the backend error if it is an `E`, or this error unchanged.
    ///
    /// # Errors
    ///
    /// Returns `self` when there is no source or it is another type.
    pub fn downcast<E: StdError + Send + Sync + 'static>(self) -> Result<Box<E>, Self> {
        match self.source {
            Some(source) => source.downcast().map_err(|source| Self {
                kind: self.kind,
                source: Some(source),
            }),
            None => Err(self),
        }
    }
}

impl From<StoreErrorKind> for StoreError {
    fn from(kind: StoreErrorKind) -> Self {
        Self { kind, source: None }
    }
}

impl fmt::Debug for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreError")
            .field("kind", &self.kind)
            .field("source", &self.source)
            .finish()
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind.describe())
    }
}

impl StdError for StoreError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

impl From<crate::ObjectFormatError> for StoreError {
    fn from(error: crate::ObjectFormatError) -> Self {
        Self::new(StoreErrorKind::InvalidInput, error)
    }
}

impl From<ObjectReadError> for StoreError {
    fn from(error: ObjectReadError) -> Self {
        let kind = match &error {
            ObjectReadError::Cancelled => StoreErrorKind::Cancelled,
            ObjectReadError::Limit(_) => StoreErrorKind::Limit,
            ObjectReadError::Unsupported(_)
            | ObjectReadError::IndexVersion(_)
            | ObjectReadError::PackVersion(_)
            | ObjectReadError::ObjectType(_) => StoreErrorKind::Unsupported,
            ObjectReadError::PackChanged { .. } => StoreErrorKind::Conflict,
            // Damaged packed entries surface as `InvalidData` once every fallback copy fails.
            ObjectReadError::Path { source, .. }
                if source.kind() == std::io::ErrorKind::InvalidData =>
            {
                StoreErrorKind::Corrupt
            }
            ObjectReadError::Path { .. } => StoreErrorKind::Unavailable,
            ObjectReadError::Loose(loose) => match loose {
                crate::loose::Error::ObjectFormat(_) => StoreErrorKind::InvalidInput,
                crate::loose::Error::TooLarge => StoreErrorKind::Limit,
                crate::loose::Error::Io(_) => StoreErrorKind::Unavailable,
                crate::loose::Error::UnknownObjectType => StoreErrorKind::Unsupported,
                crate::loose::Error::ConflictingObject => StoreErrorKind::Conflict,
                _ => StoreErrorKind::Corrupt,
            },
            _ => StoreErrorKind::Corrupt,
        };
        Self::new(kind, error)
    }
}

/// A repository's object directory is a store; reads keep their filesystem errors as the source.
///
/// Reads, headers and existence checks call [`Objects::read_controlled`], [`Objects::header`] and
/// [`Objects::contains`]. See the [module documentation](self) for when a read leaves the task.
impl ObjectRead for Objects {
    fn object_format(&self) -> ObjectFormat {
        Objects::object_format(self)
    }

    async fn read(&self, id: ObjectId, limits: ReadLimits) -> Result<Option<Object>, StoreError> {
        #[cfg(feature = "tokio")]
        if let Ok(runtime) = tokio::runtime::Handle::try_current()
            && limits.max_object_bytes as u64 > INLINE_PAYLOAD_BYTES
        {
            // Stored sizes are checked before inflating, so a large object is refused after its
            // header; a separate header lookup would cost a small loose read about 15 µs more.
            let inline = limits.with_max_object_bytes(INLINE_PAYLOAD_BYTES as usize);
            return match self.read_controlled(id, inline, &AtomicBool::new(false)) {
                Err(
                    ObjectReadError::Limit(_)
                    | ObjectReadError::Loose(crate::loose::Error::TooLarge),
                ) => offload(&runtime, self.clone(), id, limits).await,
                result => Ok(result?),
            };
        }
        Ok(self.read_controlled(id, limits, &AtomicBool::new(false))?)
    }

    async fn header(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<ObjectHeader>, StoreError> {
        Ok(Objects::header(self, id, limits)?)
    }

    async fn contains(&self, id: ObjectId) -> Result<bool, StoreError> {
        Ok(Objects::contains(self, id)?)
    }
}

/// Reads on a blocking thread; dropping the returned future tells the read to stop.
#[cfg(feature = "tokio")]
async fn offload(
    runtime: &tokio::runtime::Handle,
    objects: Objects,
    id: ObjectId,
    limits: ReadLimits,
) -> Result<Option<Object>, StoreError> {
    struct CancelOnDrop(Arc<AtomicBool>);
    impl Drop for CancelOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let cancel = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(Arc::clone(&cancel));
    let read = runtime.spawn_blocking(move || objects.read_controlled(id, limits, &cancel));
    let result = read
        .await
        .map_err(|error| StoreError::new(StoreErrorKind::Unavailable, error))?;
    Ok(result?)
}

/// The filesystem store with every read inline, for synchronous entry points.
///
/// [`block_on`] over this never leaves the thread, even inside a Tokio runtime.
pub(crate) struct InlineObjects<'a>(pub(crate) &'a Objects);

impl ObjectRead for InlineObjects<'_> {
    fn object_format(&self) -> ObjectFormat {
        self.0.object_format()
    }

    async fn read(&self, id: ObjectId, limits: ReadLimits) -> Result<Option<Object>, StoreError> {
        Ok(self
            .0
            .read_controlled(id, limits, &AtomicBool::new(false))?)
    }

    async fn header(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<ObjectHeader>, StoreError> {
        Ok(self.0.header(id, limits)?)
    }

    async fn contains(&self, id: ObjectId) -> Result<bool, StoreError> {
        Ok(self.0.contains(id)?)
    }
}

/// Objects held in memory, for tests, staging and tools that never touch `.git/`.
///
/// Writes hash the payload and keep one shared copy per identity; reads return that copy without
/// copying it. As in a repository, the empty tree is present even when never written. Clones
/// share the same objects.
///
/// ```
/// use girt::storage::{MemoryObjects, ObjectRead, ObjectWrite, block_on};
/// use girt::{EntryMode, ObjectFormat, ObjectKind, Tree, TreeEntry};
///
/// let store = MemoryObjects::new(ObjectFormat::Sha256);
/// let blob = block_on(store.write(ObjectKind::Blob, b"hello\n"))?;
/// let entry = TreeEntry {
///     mode: EntryMode::Blob,
///     name: b"hello.txt".to_vec(),
///     id: blob,
/// };
/// let tree = Tree::new(ObjectFormat::Sha256, vec![entry])?;
/// let id = block_on(store.write(ObjectKind::Tree, &tree.encode()))?;
///
/// assert_eq!(id, tree.id());
/// assert!(block_on(store.contains(id))?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct MemoryObjects {
    format: ObjectFormat,
    objects: Arc<RwLock<HashMap<ObjectId, Object>>>,
}

impl MemoryObjects {
    /// Creates an empty store for identities in `format`.
    pub fn new(format: ObjectFormat) -> Self {
        Self {
            format,
            objects: Arc::default(),
        }
    }

    fn get(&self, id: ObjectId) -> Result<Option<Object>, StoreError> {
        id.require_format(self.format)?;
        let objects = self.objects.read().unwrap_or_else(PoisonError::into_inner);
        if let Some(object) = objects.get(&id) {
            return Ok(Some(object.clone()));
        }

        Ok((id == self.format.empty_tree()).then(|| Object {
            kind: ObjectKind::Tree,
            format: self.format,
            data: Arc::default(),
        }))
    }
}

impl ObjectRead for MemoryObjects {
    fn object_format(&self) -> ObjectFormat {
        self.format
    }

    async fn read(&self, id: ObjectId, limits: ReadLimits) -> Result<Option<Object>, StoreError> {
        let object = self.get(id)?;
        if object
            .as_ref()
            .is_some_and(|object| object.data().len() > limits.max_object_bytes)
        {
            return Err(StoreError::new(StoreErrorKind::Limit, "object bytes"));
        }
        Ok(object)
    }

    async fn header(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<ObjectHeader>, StoreError> {
        let header = self.get(id)?.map(|object| ObjectHeader {
            kind: object.kind(),
            size: object.data().len() as u64,
        });
        if header.is_some_and(|header| header.size() > limits.max_object_bytes as u64) {
            return Err(StoreError::new(StoreErrorKind::Limit, "object bytes"));
        }
        Ok(header)
    }

    async fn contains(&self, id: ObjectId) -> Result<bool, StoreError> {
        Ok(self.get(id)?.is_some())
    }
}

impl ObjectWrite for MemoryObjects {
    async fn write(&self, kind: ObjectKind, data: &[u8]) -> Result<ObjectId, StoreError> {
        let id = self.format.hash_object(kind, data);
        let mut objects = self.objects.write().unwrap_or_else(PoisonError::into_inner);
        objects.entry(id).or_insert_with(|| Object {
            kind,
            format: self.format,
            data: Arc::new(data.to_vec()),
        });
        Ok(id)
    }
}

/// The payload a store returned does not hash to the identity it was read under.
#[derive(Debug, thiserror::Error)]
#[error("stored {} payload does not hash to {expected}", kind.as_str())]
pub struct IdentityMismatch {
    /// The identity that was requested.
    pub expected: ObjectId,
    /// The kind the store reported.
    pub kind: ObjectKind,
}

impl Object {
    /// Builds an object from a store's payload, after checking that it hashes to `id`.
    ///
    /// This is how an [`ObjectRead`] implementation outside girt returns objects. The payload is
    /// taken as is; pass an `Arc` to share it with a cache.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreErrorKind::Corrupt`] error, with an [`IdentityMismatch`] source, when the
    /// kind and payload hash to another identity.
    ///
    /// ```
    /// use girt::{Object, ObjectFormat, ObjectKind};
    ///
    /// let id = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"data");
    /// let object = Object::verified(id, ObjectKind::Blob, b"data".to_vec())?;
    /// assert_eq!(object.id(), id);
    ///
    /// assert!(Object::verified(id, ObjectKind::Blob, b"other".to_vec()).is_err());
    /// # Ok::<(), girt::storage::StoreError>(())
    /// ```
    pub fn verified(
        id: ObjectId,
        kind: ObjectKind,
        data: impl Into<Arc<Vec<u8>>>,
    ) -> Result<Self, StoreError> {
        let object = Self {
            kind,
            format: id.format(),
            data: data.into(),
        };
        if object.id() != id {
            return Err(StoreError::new(
                StoreErrorKind::Corrupt,
                IdentityMismatch { expected: id, kind },
            ));
        }
        Ok(object)
    }
}

#[cfg(test)]
mod tests;
