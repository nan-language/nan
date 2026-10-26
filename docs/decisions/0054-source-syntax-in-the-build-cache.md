# 0054. Source syntax in the build cache

Status: accepted

## Context

Every command that checks, formats, shows or builds parses the source files it loads: the workspace's and the 275 KB of std sources the toolchain carries. The NaN toolchain's table-driven parser takes about 130 ms for std once self-built, half of a warm `nan check` of a one-package workspace, which the bootstrap runs in 110 ms; parsing is also a fixed share of every check of a large workspace, whatever the query engine reuses. The syntax of a file depends only on its bytes, on whether it is a std file, and on the toolchain, whose source tree holds the grammar and the parser.

## Decision

- **The build cache holds the syntax of source files.** Under a key of the entry's kind, the toolchain hash, whether the file is parsed as a std file and the hash of its bytes, an entry holds what parsing the file gives: its tokens, lines, items with their trees, comment marks and diagnostics. Loading a source file decodes its entry when the cache holds one, and otherwise parses the file and writes the entry.
- **One provider gives every loaded source file its syntax:** workspace files read from disk or from the local store, and the std files the toolchain carries. In-memory overlays parse.
- **Only an image that records its source tree hash uses the cache.** Without a `.nan.tree` section ([0052](0052-the-source-tree-hash-recorded-in-the-image.md)), finding the toolchain hash takes a second, which a command that needs no proof would pay for nothing; such a toolchain parses.
- **Decoding stands for parsing exactly.** An entry that does not decode is absent, and the file is parsed.

Rejected alternatives:

- **A faster parser alone:** after its tables and lookups were tightened, parsing std still took longer than the bootstrap's whole command.
- **One entry for all std files:** it leaves workspace files to parse at every run.
- **Parsing only the std packages a command reads:** every check reads every std package's items to resolve names.

## Consequences

- A command decodes the syntax of the files it loads instead of parsing them; it hashes their bytes to find the entries.
- The first command after a toolchain change or a file edit parses the changed files and writes their entries.
- The first stage, built by the bootstrap, never uses these entries; the self-built toolchain does.
- The encoding of syntax is the toolchain's own and changes with it; the toolchain hash keeps entries of other toolchains apart.
