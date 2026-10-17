---
title: Supported and refused
description: What girt supports, what it refuses, and why it refuses instead of approximating.
---

:::note[Work in progress]
This page is an outline. The
[compatibility evidence](https://github.com/joshka/girt/blob/main/docs/compatibility.md) in the
repository records every supported boundary and the tests behind it. This page will summarize it for
users.
:::

girt supports a feature only when it can reproduce Git's format and semantics exactly. Anything
outside that boundary is refused with an error that names it. A refusal tells you to use another
tool for that repository. A silent approximation could write data that Git reads differently.

## Outline

- **Supported:** object formats, storage, reference backends, index versions, transports and
  platforms, each with a link to its evidence.
- **Refused:** unknown repository extensions, partial clone and filters, submodule recursion,
  `git-remote-*` helpers, attributes, filters and end-of-line conversion in status and checkout, and
  Windows status and checkout.
- **Documented differences:** behaviour that is compatible but not byte-identical, such as
  compressed object bytes and GC packing.
- **How refusals surface:** error types and how to detect a refusal programmatically.
- **How compatibility is established:** tests against the `git` executable, independent fixtures,
  and the platforms and Git versions covered in CI.
