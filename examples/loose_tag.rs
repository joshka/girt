//! Run with `cargo run --example loose_tag`; all storage is disposable.
use girt::{LooseObjects, ObjectFormat, ObjectKind, Signature, Tag, TagFields};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let objects = LooseObjects::new(directory.path().join("objects"), ObjectFormat::Sha1)?;
    let target = objects.write_blob(b"Release artifact\n")?;
    let tag = Tag::new(TagFields {
        target,
        target_kind: ObjectKind::Blob,
        name: b"v1".to_vec(),
        tagger: Some(Signature {
            name: b"A. Releaser".to_vec(),
            email: b"release@example.com".to_vec(),
            seconds: 1_700_000_000,
            offset_minutes: -420,
        }),
        extra_headers: vec![],
        message: b"First release\n".to_vec(),
    })?;
    let id = objects.write_tag(&tag)?;
    let restored = objects.read_tag(id, 4096)?;
    assert_eq!(restored, tag);
    assert_eq!(
        objects.read_blob(restored.fields().target, 1024)?,
        b"Release artifact\n"
    );
    println!("Stored annotated tag object {id} targeting blob {target}");
    // No tag reference is created. All temporary objects are removed on normal exit.
    Ok(())
}
