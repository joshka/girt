//! Resolve a hexadecimal prefix and print a unique, automatically sized abbreviation.
use girt::{ObjectIdPrefix, PackLimits, Repository};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: short_ids <repository> <hex-prefix>")?;
    let text = args.next().ok_or("missing hexadecimal prefix")?;
    let repository = Repository::discover(path)?;
    let objects = repository.objects(PackLimits::default())?;
    let prefix = ObjectIdPrefix::from_hex(repository.object_format(), &text)?;
    let id = objects.resolve_prefix(prefix)?;
    println!("{}", objects.abbreviate(id, objects.auto_abbrev_len())?);
    Ok(())
}
