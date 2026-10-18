//! Expected values were checked against `git archive` on the same objects.
use crate::archive::{Error, ExtraFile, Format, Options};
use crate::pathspec::{Context, Pathspec};
use crate::tree::TreeEditor;
use crate::{Commit, InitKind, ObjectFormat, ObjectId, Repository};

struct Fixture {
    _directory: tempfile::TempDir,
    repo: Repository,
    commit: ObjectId,
}

/// A bare repository whose commit holds `files`, committed at 1700000000 +0000.
fn fixture(files: &[(&str, &[u8])]) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("repo.git"),
        InitKind::Bare,
    )
    .unwrap();
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    for (path, content) in files {
        editor
            .set(path.as_bytes(), loose.write_blob(content).unwrap())
            .unwrap();
    }
    let tree = editor.write().unwrap();
    let payload = format!(
        "tree {tree}\nauthor A U <a@example.com> 1700000000 +0000\n\
         committer C O <c@example.com> 1700000000 +0000\n\nSubject line\n\nBody\n"
    );
    let commit = Commit::parse(ObjectFormat::Sha1, payload.as_bytes()).unwrap();
    let commit = loose.write_commit(&commit).unwrap();
    Fixture {
        _directory: directory,
        repo,
        commit,
    }
}

/// The content of `path` in a tar archive.
fn tar_file(tar: &[u8], path: &str) -> Option<Vec<u8>> {
    let mut at = 0;
    while let Some(header) = tar.get(at..at + 512) {
        let name_end = header
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(100)
            .min(100);
        let size = std::str::from_utf8(&header[124..135]).ok()?;
        let size = usize::from_str_radix(size, 8).ok()?;
        if header.iter().all(|&byte| byte == 0) {
            return None;
        }
        if &header[..name_end] == path.as_bytes() {
            return tar.get(at + 512..at + 512 + size).map(<[u8]>::to_vec);
        }
        at += 512 + size.div_ceil(512) * 512;
    }
    None
}

fn archive(fixture: &Fixture, options: &Options) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    fixture
        .repo
        .archive(fixture.commit, &Pathspec::all(), options, &mut out)?;
    Ok(out)
}

#[test]
fn expands_export_subst_placeholders() {
    let fixture = fixture(&[
        (".gitattributes", b"v export-subst\n"),
        (
            "v",
            b"$Format:%s|%an|%ce|%ad|%ci|%ct|%%|%n$ $Format:%H$ %H\n",
        ),
    ]);
    let tar = archive(&fixture, &Options::new()).unwrap();
    let expected = format!(
        "Subject line|A U|c@example.com|Tue Nov 14 22:13:20 2023 +0000|\
         2023-11-14 22:13:20 +0000|1700000000|%|\n {} %H\n",
        fixture.commit
    );
    assert_eq!(tar_file(&tar, "v").unwrap(), expected.as_bytes());
}

#[test]
fn refuses_unsupported_placeholders() {
    let fixture = fixture(&[
        (".gitattributes", b"v export-subst\n"),
        ("v", b"$Format:%d$\n"),
    ]);
    let error = archive(&fixture, &Options::new()).unwrap_err();
    assert!(
        matches!(&error, Error::Placeholder { placeholder, .. } if placeholder == "%d"),
        "{error:?}"
    );
}

#[test]
fn leaves_files_without_the_attribute_alone() {
    let fixture = fixture(&[("v", b"$Format:%H$\n")]);
    let tar = archive(&fixture, &Options::new()).unwrap();
    assert_eq!(tar_file(&tar, "v").unwrap(), b"$Format:%H$\n");
}

#[test]
fn records_the_commit_in_a_pax_global_header() {
    let fixture = fixture(&[("a", b"a\n")]);
    let tar = archive(&fixture, &Options::new()).unwrap();
    assert!(tar.starts_with(b"pax_global_header\0"));
    let record = format!("52 comment={}\n", fixture.commit);
    assert_eq!(&tar[512..512 + record.len()], record.as_bytes());
    assert_eq!(tar.len(), 10240);
}

#[test]
fn drops_export_ignore_paths() {
    let fixture = fixture(&[
        (".gitattributes", b"secret export-ignore\n"),
        ("secret", b"s\n"),
        ("public", b"p\n"),
    ]);
    let tar = archive(&fixture, &Options::new()).unwrap();
    assert!(tar_file(&tar, "secret").is_none());
    assert_eq!(tar_file(&tar, "public").unwrap(), b"p\n");
}

#[test]
fn reports_an_unmatched_pathspec() {
    let fixture = fixture(&[("a", b"a\n")]);
    let pathspec = Pathspec::parse(["missing"], &Context::new()).unwrap();
    let error = fixture
        .repo
        .archive(fixture.commit, &pathspec, &Options::new(), &mut Vec::new())
        .unwrap_err();
    assert!(matches!(error, Error::NoMatch), "{error:?}");
}

#[test]
fn adds_extra_files_after_the_tree() {
    let fixture = fixture(&[("a", b"a\n")]);
    let options = Options::new()
        .with_format(Format::Zip)
        .with_extra_file(ExtraFile::new("notes/extra", "x").with_executable(true));
    let zip = archive(&fixture, &options).unwrap();
    let names: Vec<_> = zip
        .windows(4)
        .enumerate()
        .filter(|(_, window)| *window == b"PK\x01\x02")
        .map(|(at, _)| {
            let length = usize::from(u16::from_le_bytes([zip[at + 28], zip[at + 29]]));
            String::from_utf8_lossy(&zip[at + 46..at + 46 + length]).into_owned()
        })
        .collect();
    assert_eq!(names, ["a", "notes/extra"]);
    assert!(zip.ends_with(fixture.commit.to_string().as_bytes()));
}
