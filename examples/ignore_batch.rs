//! A bounded caller-owned loader and byte-path batch adapter for the ignore benchmark.
//! Input paths are NUL-separated file paths; source content is one root .gitignore.
use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Parser;
use girt::ignore::{Case, Ignore, InputKind, Limits, Source};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Root .gitignore file whose rules are applied to the NUL-separated paths on stdin.
    file: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let limits = Limits::default();
    let mut content = Vec::new();
    std::fs::File::open(args.file)?
        .take(limits.bytes as u64 + 1)
        .read_to_end(&mut content)?;
    let mut rules = Ignore::new(Case::Sensitive, limits);
    rules.add(Source::Directory(b""), &content)?;
    let mut input = Vec::new();
    std::io::stdin()
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut input)?;
    if input.len() > 16 * 1024 * 1024 {
        return Err("batch exceeds 16 MiB".into());
    }
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    for path in input.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        if rules
            .check(path, InputKind::File)?
            .is_some_and(|m| m.ignored)
        {
            output.write_all(path)?;
            output.write_all(&[0])?;
        }
    }
    output.flush()?;
    Ok(())
}
