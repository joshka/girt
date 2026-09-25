//! A caller-owned scoped subscriber; the library installs no process-wide logging policy.
use girt::{InitKind, Repository};
use tracing_subscriber::fmt::format::FmtSpan;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // TRACE includes individual storage reads/writes. Use DEBUG for workflow/phase summaries.
    // The caller chooses the sink, filtering and whether closed spans should produce diagnostics.
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(FmtSpan::CLOSE)
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    tracing::dispatcher::with_default(&dispatch, || -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let repo = Repository::init(
            girt::ObjectFormat::Sha1,
            root.path().join("repo"),
            InitKind::Bare,
        )?;
        let loose = repo.loose_objects();
        let id = loose.write_blob(b"caller-owned tracing example")?;
        assert_eq!(loose.read_blob(id, 1024)?, b"caller-owned tracing example");
        repo.edit_index(Default::default())?.commit()?;
        Ok(())
    })
}
