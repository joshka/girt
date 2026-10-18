# Present canonical commit identities

Use a mailmap when one contributor has committed under several names or email addresses. Mapping
changes display values; the original commit bytes and object IDs remain unchanged.

Parse caller-owned bytes with `girt::mailmap::Mailmap::parse`, then resolve the name and email from
an author or committer. `resolve_signature` preserves the signature's original date bytes. Use
`Mailmap::load(&repository, &objects)` to read the repository's captured configuration and sources.

```rust
use girt::mailmap::Mailmap;

let map = Mailmap::parse(b"A. Writer <writer@example.com> <old@example.com>\n");
let author = map.resolve(b"Writer", b"old@example.com");
assert_eq!(author.name, b"A. Writer");
assert_eq!(author.email, b"writer@example.com");
```

Mailmap accepts these four forms:

```text
Canonical Name <old@example.com>
<canonical@example.com> <old@example.com>
Canonical Name <canonical@example.com> <old@example.com>
Canonical Name <canonical@example.com> Old Name <old@example.com>
```

The first form changes only the name. The second changes only the email. The third changes both for
everyone using the old email. The fourth applies only to the specified old name and email. Names and
emails match without ASCII case sensitivity. Matching does not normalize Unicode. Blank lines,
comments beginning with `#`, and malformed lines are ignored. Later entries replace only supplied
components; name-specific entries take precedence over email-only entries.

Repository loading reads the worktree `.mailmap`, then `mailmap.blob`, then `mailmap.file`. Bare
repositories default the blob to `HEAD:.mailmap`. Missing sources are ignored; corrupt objects and
unreadable files return errors. Relative file paths are rooted at the worktree or bare repository.
Home-prefixed configured paths require caller expansion and are refused by this loader; callers can
instead load those bytes and call `extend`.

The CLI log maps authors by default, following `log.mailmap`. Select explicitly with
`girt log --use-mailmap` or `--use-mailmap=false`. `girt blame -e` prefixes each structured range
with its canonical author email. Library history and blame results retain raw objects and origin
IDs; callers choose when to load and apply the map. The CLI does not implement shortlog.

The CLI expands `~/` using its current home directory. Named-user `~user/` paths remain explicitly
refused and tracked in [issue 555](https://github.com/joshka/girt/issues/555).
