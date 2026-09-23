# Blob Compatibility Evidence

The first slice supports canonical SHA-1 blobs in loose storage. The caller supplies an object
directory and its known `ObjectFormat::Sha1` format. The library rejects `ObjectFormat::Sha256`; it
does not read repository configuration. Crate Rustdoc owns the API examples and complete
limitations.

## References and Provenance

The format references are [Git Objects](https://git-scm.com/book/en/v2/Git-Internals-Git-Objects)
and [Git's hash transition document](https://git-scm.com/docs/hash-function-transition). Blob
identity hashes the uncompressed header and content. Loose files contain a zlib stream, located
under the first two hexadecimal identity digits and a filename containing the remaining digits.

Implementation and tests are original. No Git source, test code, or comments were copied or adapted.
`tests/blobs.rs` generates all fixtures at runtime: an empty blob, an original text string, repeated
bytes spanning 0 through 255, and deliberately malformed encodings. No fixture files are vendored.

Run `cargo test` with Git available on `PATH`. Interoperability tests create isolated bare SHA-1
repositories using `git init --bare --object-format=sha1 --template=`. They compare identities with
`git hash-object --stdin`, read girt-written objects with `git cat-file blob`, then remove those
loose files and regenerate them using `git hash-object -w --stdin` for girt to read. Tests remove
inherited `GIT_*` environment overrides and disable system/global config for child processes. They
do not modify global configuration or the working checkout. This slice was validated with Git 2.55.0
on macOS; other platforms have not been exercised.

## Validation Boundaries

Tests cover empty and binary content, exact read limits, missing objects, unsupported formats and
object types, malformed headers and lengths, wrong identities, every truncation of a small
compressed object, invalid checksums, trailing bytes, and concatenated streams. Publication tests
cover repeated and concurrent writes, existing corrupt objects, and failed publication with
temporary-file cleanup.

Reads require canonical decimal lengths and complete zlib streams. They reject malformed encodings
rather than emulate Git's tolerance for unusual inputs. Compressed bytes can differ from Git's
output; identity and decoded bytes must agree. This slice does not implement collision detection,
packed objects, alternates, power-loss durability, shared-repository permissions, or protection
against a hostile filesystem owner. File publication requires hard-link support.

## Dependencies

Direct dependencies are `sha1` for hashing, `flate2` for zlib, and `tempfile` for exclusive
temporary files and disposable test directories. All three declare `MIT OR Apache-2.0`. Requirements
permit compatible releases within their selected release series; the lockfile records tested
versions.

The resolved dependency manifests were checked with `cargo metadata --format-version 1`. Every
resolved package offers MIT, Apache-2.0, or Zlib terms, including platform-specific dependencies.
`simd-adler32` and `generic-array` use MIT; `zlib-rs` uses Zlib. License alternatives in transitive
packages do not require selecting LGPL. This records package metadata, not an independent legal
audit.
