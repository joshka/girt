# Rust Conventions

Use these conventions alongside the [Rustdoc Standard](rustdoc.md) and the repository's
maintainability and testing guidance.

## Module Roots and Ownership

- Treat `lib.rs` and directory-root modules primarily as tables of contents: module declarations,
  re-exports, and a concise overview.
- Put definitions in files named for the concepts they own.
- Re-export items where that makes the public API easier to navigate. Public modules should provide
  useful namespaces rather than expose every implementation file.
- Keep detailed contracts, examples, and limitations with their owning type or operation. A reader
  arriving directly at a type should not need the crate overview to understand its obligations.
- Keep filesystem assumptions and storage errors with loose storage; link to those contracts from
  the crate root.

## Domain Types at API Boundaries

- Use domain types when a primitive would admit invalid choices, obscure meaning, or allow unrelated
  values to be interchanged.
- Use an enum for a finite set of meaningful alternatives, such as Git object formats.
- Use a newtype when a value needs validated construction or an identity distinct from its
  underlying representation, such as an object identifier.
- Keep primitive types when their meaning and valid values are already clear and a wrapper would add
  no useful guarantee.
- Parse external strings at the boundary, then pass the domain type through library operations.
- Avoid magic strings such as `"sha1"` to select object behavior. A newtype around an unchecked
  string does not solve that problem.
- Distinguish recognized values from supported operations. Explicitly reject a recognized format
  when the storage implementation does not support it.
- Do not introduce an extensible format registry or speculative configuration framework to represent
  a small closed choice.
