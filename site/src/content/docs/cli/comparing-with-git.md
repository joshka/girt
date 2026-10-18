---
title: Comparing with Git
description: Use the CLI as a testing surface for library behavior and performance.
---

The CLI lets you run a library operation against the same repository as Git to investigate behavior
or measure performance. This is an optional testing workflow; start with
[Getting started with the CLI](/girt/cli/getting-started/) to learn the commands and create an
example repository.

:::caution[Experimental CLI]
The girt CLI is not a replacement for Git. It exercises the library's Git operations and provides a
testing surface for performance and byte-for-byte comparisons with Git. Command names and parameters
may change; the library is the core deliverable.
:::

## Choose corresponding operations

The command pages list Git equivalents to help you find the same operation. girt organizes commands
around library concepts: `girt tree list` corresponds to `git ls-tree`, and `girt ref set`
corresponds to `git update-ref`. Names, flags and default output can differ intentionally. Matching
Git's entire CLI is not a requirement for the library or this experimental front end.

## Compare bytes when useful

`--git` selects Git-compatible output where a command supports it. For example, in Bash or Zsh:

```sh
diff <(girt --git tree list --recursive HEAD) <(git ls-tree -r HEAD)
```

An empty diff means this tree listing matches. The option makes byte-for-byte checks convenient; it
does not establish compatibility for every operation. Blame uses structured ranges and refuses
`--git`. Clone, fetch and push keep their own result format, and maintenance has no exact Git
counterpart. Use the command's documented semantics to interpret results.

## Measure performance

Time the same operation on the same repository and revision, with equivalent options. For example,
if Hyperfine is installed:

```sh
hyperfine 'girt --git tree list --recursive HEAD' 'git ls-tree -r HEAD'
```

Keep repository contents fixed and record the versions, options and cache conditions with the
result. Use disposable copies for operations that write, and reset them between runs. Otherwise a
later run may do less work because an earlier run already changed the repository.

Errors and refusals describe the library's supported boundary; they are not timing results for a
completed operation. See [Limits and trust](/girt/guides/limits-and-trust/) for those boundaries.
