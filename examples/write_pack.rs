//! Run `cargo run --example write_pack`; exports and verifies an explicit set in private storage.
use std::fs::{self, File};
use std::io::{BufWriter, Write};

use girt::{
    ObjectId, ObjectKind, PackLimits, PackObject, PackWriteLimits, ReadLimits, Repository,
    write_pack,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let format = girt::ObjectFormat::Sha256;
    let payload = b"explicit export\0\xff";
    let id = ObjectId::for_blob(format, payload);
    let inputs = [PackObject {
        id,
        kind: ObjectKind::Blob,
        data: payload,
    }];
    let mut pack = BufWriter::new(File::create(root.path().join("export.pack"))?);
    let mut index = BufWriter::new(File::create(root.path().join("export.idx"))?);
    let written = write_pack(
        format,
        &inputs,
        &mut pack,
        &mut index,
        PackWriteLimits::default(),
    )?;
    pack.flush()?;
    index.flush()?;
    drop((pack, index));

    // Only this example owns the new repository. Prepare both files before opening any readers.
    // A live repository needs a separate safe publication protocol, not this two-rename sequence.
    let repo_path = root.path().join("private.git");
    let repo = Repository::init(format, &repo_path, girt::InitKind::Bare)?;
    let basename = format!("objects/pack/pack-{}", written.checksum);
    fs::rename(
        root.path().join("export.pack"),
        repo_path.join(format!("{basename}.pack")),
    )?;
    fs::rename(
        root.path().join("export.idx"),
        repo_path.join(format!("{basename}.idx")),
    )?;
    let objects = repo.objects(PackLimits::default())?;
    let restored = objects
        .read(id, ReadLimits::default())?
        .ok_or("exported object missing")?;
    assert_eq!(restored.kind(), ObjectKind::Blob);
    assert_eq!(restored.data(), payload);
    println!(
        "Verified {} explicit object in {} pack bytes and {} index bytes",
        written.objects, written.pack_bytes, written.index_bytes
    );
    Ok(())
}
