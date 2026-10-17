---
title: girt as jj's Git backend
description: How jj runs on girt, what works, and how to build and try it.
---

:::note[Work in progress]
This page is an outline. The repository's
[jj on girt status](https://github.com/joshka/girt/blob/main/docs/jj-replacement-status.md) is the
current source.
:::

An experimental jj branch,
[`joshka/girt-backend`](https://github.com/joshka/jj/tree/joshka/girt-backend), replaces gix,
gix-ignore and the Git subprocess in jj with girt. It isn't proposed upstream. jj's production code
on that branch has no gix, libgit2 or Git subprocess use. It passes jj's workspace test suite, an
end-to-end script that alternates jj and Git operations, and a check that no `git` process runs.

## Outline

- **What works:** init, colocation, commit, export and import, clone, fetch, push, workspaces and
  GC, with SHA-1 and SHA-256.
- **Known limitations:** Windows is untested, and partial clone, submodule recursion, `git-remote-*`
  helpers and some credential helpers aren't supported.
- **Building jj on girt:** the branch, pinning a girt release, and testing an unreleased girt.
- **Acceptance checks:** the test suite, the end-to-end script and the no-Git tripwire.
- **Performance:** see the [jj on girt benchmark](/girt/performance/jj-benchmark/).
- **How jj uses girt:** which girt APIs jj calls, as a worked example of the library.
