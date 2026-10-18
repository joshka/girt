#!/usr/bin/env python3
"""Find Rust code that runs on without blank lines between its steps.

Readers see a blank line as the edge of a group of related statements. A long run of statements
with no gap hides those groups, however clearly each line reads. This script reports such runs in
function bodies, Rustdoc examples, runnable examples and the website guides.

A paragraph is a sequence of statements in one block with no blank line between them. Lines nested
inside a statement, such as a multi-line call or the body of an `if`, count toward the statement's
paragraph but do not add statements; the nested block's own statements are measured separately.
A single long expression is therefore never flagged, and neither are `match` arms, struct literals
or items. The script relies on rustfmt indentation rather than parsing Rust.

    scripts/code-spacing.py            # summary per surface
    scripts/code-spacing.py --list     # every flagged paragraph, longest first
    scripts/code-spacing.py --json     # machine-readable summary for the quality scorecard
    scripts/code-spacing.py src/refs   # restrict to paths under the given prefixes
"""

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# A paragraph with more lines than this and at least two statements is flagged.
MAX_LINES = 10

# Lines ending a block opener that holds statements, as opposed to `match` arms, struct literals,
# items or data. Matched against the line with trailing comments removed.
STATEMENT_BLOCK = re.compile(
    r"""(
        \bfn\b.*\{$                                  # function body
      | ^\s*(\}\s*else\s+)?(if|while|for|loop)\b.*\{$  # control flow
      | \belse\s*\{$                                 # else and let-else
      | ^\s*(unsafe\s+|async\s+(move\s+)?)?\{$       # bare, unsafe and async blocks
      | =>\s*(unsafe\s+)?\{$                         # match arm body
      | \|[^|]*\|(\s*->\s*[^{]+)?\s*\{$              # closure body
      | \bmove\s*\{$
    )""",
    re.VERBOSE,
)
CLOSERS = ("}", ")", "]")
# The start of a function or control-flow header whose `{` may follow on a later line, as rustfmt
# writes long signatures, `where` clauses and wrapped conditions.
HEADER = re.compile(r"^\s*((pub(\([^)]*\))?\s+)?((const|async|unsafe|extern\s+\"[^\"]*\")\s+)*fn\b"
                    r"|(\}\s*else\s+)?(if|while|for)\b)")
FUNCTION = re.compile(r"^\s*(pub(\([^)]*\))?\s+)?((const|async|unsafe|extern\s+\"[^\"]*\")\s+)*fn\b")


def indent(line):
    return len(line) - len(line.lstrip(" "))


def scan(line, state):
    """Return the code outside strings and comments, and the string state at the end of the line.

    `state` is None outside a string, `""` inside an ordinary string or the closing `"#...` of a
    raw string.
    """
    code, index = [], 0
    while index < len(line):
        if state is not None:
            if state == "" and line[index] == "\\":
                index += 2
                continue
            if line.startswith('"' + state[1:], index) if state else line[index] == '"':
                index += len(state) if state else 1
                state = None
                code.append('""')
                continue
            index += 1
            continue
        if line.startswith("//", index):
            break
        char = re.match(r"'(\\.|[^\\'])'", line[index:])
        if char:
            code.append("' '")
            index += len(char.group(0))
            continue
        raw = re.match(r'b?r(#*)"', line[index:])
        if raw and (index == 0 or not (line[index - 1].isalnum() or line[index - 1] == "_")):
            state = '"' + raw.group(1)
            index += len(raw.group(0))
            continue
        if line[index] == '"':
            state = ""
            index += 1
            continue
        code.append(line[index])
        index += 1
    return "".join(code).rstrip(), state


def code_part(line):
    """The line without strings' contents or a trailing `//` comment."""
    return scan(line, None)[0]


def paragraphs(lines, base_indent=None):
    """Return paragraphs in statement blocks as (start index, line count, statement count), and
    the indexes of function header lines.

    `base_indent` treats the whole snippet as a block body at that indentation, for doctests.
    """
    found, functions = [], []
    # Each open block: [body indent, paragraph start, lines, statements].
    stack = []
    if base_indent is not None:
        stack.append([base_indent, None, 0, 0])

    def close(block):
        if block[1] is not None:
            found.append((block[1], block[2], block[3]))
        block[1:] = [None, 0, 0]

    pending_opener = None
    pending_header = None
    blank = False
    string = None
    for number, line in enumerate(lines):
        if string is not None:
            # Inside a multi-line string literal: part of the current statement, nothing more.
            for block in stack:
                if block[1] is not None:
                    block[2] += 1
            string = scan(line, string)[1]
            continue
        if not line.strip():
            blank = True
            continue
        here = indent(line)
        code, string = scan(line, None)
        if pending_opener is not None:
            opener_indent = pending_opener
            pending_opener = None
            if here == opener_indent + 4:
                stack.append([here, None, 0, 0])
                blank = False
        while stack and here < stack[-1][0]:
            close(stack.pop())
        if blank:
            # A blank line separates statements of the block at this indentation only; deeper
            # blocks were closed above and shallower ones see it inside one statement.
            if stack and stack[-1][0] == here:
                close(stack[-1])
            blank = False
        for block in stack:
            if block[1] is None:
                block[1] = number
            block[2] += 1
        top = stack[-1] if stack else None
        if top and here == top[0] and code and code.strip() != "{" and not code.lstrip().startswith(CLOSERS) \
                and not line.lstrip().startswith(("//", "#[", "#!")):
            top[3] += 1
        if FUNCTION.match(code) and not code.endswith(";"):
            functions.append(number)
        if code and STATEMENT_BLOCK.search(code) and not re.search(r"\bmatch\b", code.split("=>")[-1]):
            pending_opener = here
            pending_header = None
        elif pending_header is not None and here == pending_header:
            # The header continues at its own indentation until its `{` or a terminating `;`.
            if code.endswith("{"):
                pending_opener = here
            if code.endswith(("{", ";", "}")):
                pending_header = None
        elif code and HEADER.match(code) and not code.endswith((";", "}")):
            pending_header = here
    while stack:
        close(stack.pop())
    return found, functions


def flagged(lines, base_indent=None):
    """Return the flagged paragraphs and the function header line indexes."""
    found, functions = paragraphs(lines, base_indent)
    return [(start, count, statements) for start, count, statements in found
            if count > MAX_LINES and statements >= 2], functions


def fenced_rust(text):
    """Yield (first line index, lines) for each ```rust fence in Markdown."""
    lines = text.splitlines()
    index = 0
    while index < len(lines):
        match = re.match(r"^(\s*)```rust\b", lines[index])
        if match:
            end = index + 1
            while end < len(lines) and not lines[end].strip().startswith("```"):
                end += 1
            yield index + 1, lines[index + 1:end]
            index = end
        index += 1


def doc_examples(text):
    """Yield (first line index, visible lines) for each Rust code block in doc comments."""
    lines = text.splitlines()
    block, start = None, 0
    for number, line in enumerate(lines):
        match = re.match(r"^\s*(///|//!) ?(.*)$", line)
        if not match:
            block = None
            continue
        body = match.group(2)
        if body.lstrip().startswith("```"):
            if block is None:
                info = body.strip()[3:]
                rust = info == "" or "rust" in info or info.split(",")[0] in (
                    "no_run", "ignore", "should_panic", "compile_fail", "edition2021")
                block, start = ([] if rust else None), number + 1
                if not rust:
                    block = "skip"
            else:
                if block != "skip":
                    yield start, block
                block = None
        elif block is not None and block != "skip":
            # Hidden doctest lines are not shown to readers; keep their place as blank-free code.
            hidden = re.match(r"^#(\s|$)", body.lstrip())
            block.append(None if hidden else body)
    return


def visible(lines):
    """Drop hidden doctest lines, which neither separate nor join visible paragraphs."""
    return [line for line in lines if line is not None]


SURFACES = [
    # (name, glob patterns, kind)
    ("website guides", ["site/src/content/docs/**/*.md", "site/src/content/docs/**/*.mdx"], "markdown"),
    ("repository docs", ["*.md", "docs/**/*.md"], "markdown"),
    ("Rustdoc examples", ["src/**/*.rs", "girt-cli/src/**/*.rs"], "rustdoc"),
    ("examples", ["examples/**/*.rs"], "rust"),
    ("library source", ["src/**/*.rs"], "rust"),
    ("CLI source", ["girt-cli/src/**/*.rs"], "rust"),
    ("tests and benches", ["tests/**/*.rs", "benches/**/*.rs", "girt-cli/tests/**/*.rs"], "rust"),
]


def measure(prefixes):
    surfaces = []
    for name, patterns, kind in SURFACES:
        files = sorted({path for pattern in patterns for path in ROOT.glob(pattern)})
        files = [p for p in files if not prefixes or any(
            str(p.relative_to(ROOT)).startswith(prefix.rstrip("/")) for prefix in prefixes)]
        items = []
        total = 0
        # Blocks or functions holding at least one flagged paragraph.
        dense = set()
        for path in files:
            relative = str(path.relative_to(ROOT))
            text = path.read_text()
            if kind == "markdown":
                for start, lines in fenced_rust(text):
                    total += 1
                    found = flagged(lines, 0)[0]
                    items += [(relative, start + s + 1, c, n) for s, c, n in found]
                    dense |= {(relative, start)} if found else set()
            elif kind == "rustdoc":
                for start, lines in doc_examples(text):
                    total += 1
                    shown = visible(lines)
                    # Map paragraph starts in the visible lines back to source lines.
                    source_lines = [start + i for i, line in enumerate(lines) if line is not None]
                    found = flagged(shown, 0)[0]
                    items += [(relative, source_lines[s] + 1, c, n) for s, c, n in found]
                    dense |= {(relative, start)} if found else set()
            else:
                found, functions = flagged(text.splitlines())
                total += len(functions)
                for s, c, n in found:
                    items.append((relative, s + 1, c, n))
                    # Attribute the paragraph to the nearest function opener above it.
                    dense.add((relative, max((f for f in functions if f < s), default=-1)))
        surfaces.append({"surface": name, "blocks": total, "dense_blocks": len(dense), "flagged": len(items),
                         "paragraphs": sorted(items, key=lambda item: -item[2])})
    return surfaces


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("prefixes", nargs="*", help="only measure paths under these prefixes")
    parser.add_argument("--list", action="store_true", help="list every flagged paragraph")
    parser.add_argument("--json", action="store_true", help="print a JSON summary")
    args = parser.parse_args()
    surfaces = measure(args.prefixes)
    if args.json:
        print(json.dumps([{**s, "paragraphs": [f"{f}:{n} ({c} lines, {k} statements)"
                                               for f, n, c, k in s["paragraphs"]]}
                          for s in surfaces], indent=2))
        return
    print(f"Paragraphs over {MAX_LINES} lines with two or more statements:")
    for surface in surfaces:
        unit = "blocks" if surface["surface"] in ("website guides", "repository docs", "Rustdoc examples") else "functions"
        print(f"  {surface['surface']:<18} {surface['flagged']:>5} paragraphs in {surface['dense_blocks']:>5} of "
              f"{surface['blocks']} {unit}")
        if args.list:
            for file, line, count, statements in surface["paragraphs"]:
                print(f"    {file}:{line}  {count} lines, {statements} statements")


if __name__ == "__main__":
    main()
