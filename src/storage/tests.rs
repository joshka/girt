use super::*;

fn read(store: &dyn DynObjectRead, id: ObjectId) -> Result<Option<Object>, StoreError> {
    block_on(store.read_dyn(id, ReadLimits::default()))
}

fn collect<S: Stream + Unpin>(mut stream: S) -> Vec<S::Item> {
    let mut items = Vec::new();
    while let Some(item) = block_on(std::future::poll_fn(|cx| {
        Pin::new(&mut stream).poll_next(cx)
    })) {
        items.push(item);
    }
    items
}

#[test]
fn memory_round_trips_and_deduplicates() {
    let store = MemoryObjects::new(ObjectFormat::Sha1);

    let id = block_on(store.write(ObjectKind::Blob, b"payload")).unwrap();
    assert_eq!(
        block_on(store.write(ObjectKind::Blob, b"payload")).unwrap(),
        id
    );

    let object = read(&store, id).unwrap().unwrap();
    assert_eq!(
        (object.kind(), object.data(), object.id()),
        (ObjectKind::Blob, &b"payload"[..], id)
    );
    let header = block_on(store.header(id, ReadLimits::default()))
        .unwrap()
        .unwrap();
    assert_eq!((header.kind(), header.size()), (ObjectKind::Blob, 7));
    assert_eq!(store.objects.read().unwrap().len(), 1);
}

#[test]
fn memory_clones_share_objects() {
    let store = MemoryObjects::new(ObjectFormat::Sha256);
    let id = block_on(store.clone().write(ObjectKind::Blob, b"shared")).unwrap();
    assert!(block_on(store.contains(id)).unwrap());
}

#[test]
fn memory_reports_absence_and_synthesizes_the_empty_tree() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let store = MemoryObjects::new(format);
        let absent = format.hash_object(ObjectKind::Blob, b"never written");
        assert!(read(&store, absent).unwrap().is_none());
        assert!(!block_on(store.contains(absent)).unwrap());
        let header = block_on(store.header(absent, ReadLimits::default()));
        assert!(header.unwrap().is_none());

        let empty = read(&store, format.empty_tree()).unwrap().unwrap();
        assert_eq!((empty.kind(), empty.data()), (ObjectKind::Tree, &b""[..]));
        assert_eq!(empty.id(), format.empty_tree());
    }
}

#[test]
fn memory_rejects_foreign_formats_and_oversized_reads() {
    let store = MemoryObjects::new(ObjectFormat::Sha1);
    let foreign = ObjectFormat::Sha256.hash_object(ObjectKind::Blob, b"x");
    assert_eq!(
        read(&store, foreign).unwrap_err().kind(),
        StoreErrorKind::InvalidInput
    );
    assert_eq!(
        block_on(store.contains(foreign)).unwrap_err().kind(),
        StoreErrorKind::InvalidInput
    );

    let id = block_on(store.write(ObjectKind::Blob, b"four")).unwrap();
    let limits = ReadLimits::default().with_max_object_bytes(3);
    let error = block_on(store.read(id, limits)).unwrap_err();
    assert_eq!(error.kind(), StoreErrorKind::Limit);
    let error = block_on(store.header(id, limits)).unwrap_err();
    assert_eq!(error.kind(), StoreErrorKind::Limit);
}

#[test]
fn verified_objects_must_hash_to_their_identity() {
    let id = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"data");
    assert_eq!(
        Object::verified(id, ObjectKind::Blob, b"data".to_vec())
            .unwrap()
            .id(),
        id
    );

    let error = Object::verified(id, ObjectKind::Tree, b"data".to_vec()).unwrap_err();
    assert_eq!(error.kind(), StoreErrorKind::Corrupt);
    let mismatch = error.downcast::<IdentityMismatch>().unwrap();
    assert_eq!((mismatch.expected, mismatch.kind), (id, ObjectKind::Tree));
}

#[test]
fn filesystem_errors_survive_as_sources() {
    let directory = tempfile::tempdir().unwrap();
    let repo = crate::Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r.git"),
        crate::InitKind::Bare,
    )
    .unwrap();
    let id = repo.loose_objects().write_blob(b"payload").unwrap();
    let objects = repo.objects().unwrap();

    assert_eq!(read(&objects, id).unwrap().unwrap().data(), b"payload");
    assert!(block_on(ObjectRead::contains(&objects, id)).unwrap());

    let limits = ReadLimits::default().with_max_object_bytes(3);
    let error = block_on(ObjectRead::read(&objects, id, limits)).unwrap_err();
    assert_eq!(error.kind(), StoreErrorKind::Limit);
    assert!(matches!(
        *error.downcast::<ObjectReadError>().unwrap(),
        ObjectReadError::Loose(crate::loose::Error::TooLarge)
    ));

    let foreign = ObjectFormat::Sha256.hash_object(ObjectKind::Blob, b"x");
    assert_eq!(
        read(&objects, foreign).unwrap_err().kind(),
        StoreErrorKind::InvalidInput
    );
}

#[test]
fn downcast_to_the_wrong_type_keeps_the_error() {
    let error = StoreError::new(StoreErrorKind::Unavailable, "connection reset");
    let error = error.downcast::<ObjectReadError>().unwrap_err();
    assert_eq!(error.kind(), StoreErrorKind::Unavailable);
    assert_eq!(error.source().unwrap().to_string(), "connection reset");
}

#[test]
fn read_many_yields_in_order_including_absence() {
    let store = MemoryObjects::new(ObjectFormat::Sha1);
    let first = block_on(store.write(ObjectKind::Blob, b"first")).unwrap();
    let second = block_on(store.write(ObjectKind::Blob, b"second")).unwrap();
    let absent = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"absent");
    let ids = [second, absent, first];

    let direct = collect(pin!(store.read_many(&ids, ReadLimits::default())));
    let erased: &dyn DynObjectRead = &store;
    let dynamic = collect(erased.read_many_dyn(&ids, ReadLimits::default()));

    for items in [direct, dynamic] {
        let found: Vec<_> = items
            .into_iter()
            .map(|item| item.map(|(id, object)| (id, object.map(|object| object.into_data()))))
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            found,
            [
                (second, Some(b"second".to_vec())),
                (absent, None),
                (first, Some(b"first".to_vec()))
            ]
        );
    }
}

#[test]
fn read_many_reports_each_failure_and_continues() {
    let store = MemoryObjects::new(ObjectFormat::Sha1);
    let large = block_on(store.write(ObjectKind::Blob, b"too large")).unwrap();
    let small = block_on(store.write(ObjectKind::Blob, b"ok")).unwrap();
    let limits = ReadLimits::default().with_max_object_bytes(2);

    let items = collect(pin!(store.read_many(&[large, small], limits)));
    assert_eq!(items[0].as_ref().unwrap_err().kind(), StoreErrorKind::Limit);
    assert_eq!(items[1].as_ref().unwrap().0, small);
}

#[test]
fn block_on_waits_for_wakeups_from_other_threads() {
    let (sender, receiver) = std::sync::mpsc::channel::<Waker>();
    let mut polls = 0;
    let future = std::future::poll_fn(move |cx| {
        polls += 1;
        if polls == 3 {
            return Poll::Ready(7);
        }
        sender.send(cx.waker().clone()).unwrap();
        Poll::Pending
    });
    let waker = std::thread::spawn(move || {
        // The no-op waker from the first poll is skipped; the second poll sends the real one.
        let _noop = receiver.recv().unwrap();
        receiver.recv().unwrap().wake();
    });

    assert_eq!(block_on(future), 7);
    waker.join().unwrap();
}

#[cfg(feature = "tokio")]
#[test]
fn large_filesystem_reads_leave_the_runtime_and_stop_when_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let repo = crate::Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r.git"),
        crate::InitKind::Bare,
    )
    .unwrap();
    let large = vec![b'x'; INLINE_PAYLOAD_BYTES as usize + 1];
    let large_id = repo.loose_objects().write_blob(&large).unwrap();
    let small_id = repo.loose_objects().write_blob(b"small").unwrap();
    let objects = repo.objects().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    runtime.block_on(async {
        let limits = ReadLimits::default();
        let object = ObjectRead::read(&objects, large_id, limits).await.unwrap();
        assert_eq!(object.unwrap().data(), large);
        let object = ObjectRead::read(&objects, small_id, limits).await.unwrap();
        assert_eq!(object.unwrap().data(), b"small");

        // The offloaded read is polled once, then dropped; the runtime shuts down cleanly.
        let mut read = pin!(ObjectRead::read(&objects, large_id, limits));
        let first = std::future::poll_fn(|cx| Poll::Ready(read.as_mut().poll(cx))).await;
        assert!(first.is_pending());
    });
}

#[test]
fn damaged_packed_reads_classify_as_corrupt() {
    let path = |kind| ObjectReadError::Path {
        path: "pack".into(),
        source: std::io::Error::new(kind, "entry"),
    };
    let corrupt = StoreError::from(path(std::io::ErrorKind::InvalidData));
    assert_eq!(corrupt.kind(), StoreErrorKind::Corrupt);
    let unavailable = StoreError::from(path(std::io::ErrorKind::PermissionDenied));
    assert_eq!(unavailable.kind(), StoreErrorKind::Unavailable);
}
