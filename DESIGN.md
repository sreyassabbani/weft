# Design

weft is a language server for a folder of Markdown notes that link to each
other the way Obsidian notes do. It is written for Helix first.

## Why a new server

I tried the existing ones against a copy of the vault before writing this.

**zk** (0.15.7, and `main` as of 2026-11-15) is maintained and careful, but it
is a note-taking CLI with an LSP attached, and the LSP is the thin part:

| | zk | needed |
|---|---|---|
| `[[` completion | Answers only at the `[[` itself and returns every note. A later request returns nothing, so in Helix the menu dies at the first space of a multi-word title. | Query on every keystroke. |
| Heading links | `[[Array#C++#Dynamic Allocation]]` goes to line 1 of `Array.md`. No heading index. | Go to the heading, complete headings, rename them. |
| References | One location per linking note, at column 0 of a line found by string search. | Exact ranges. |
| Unresolved links | No definition, hover, or references. | First-class. See below. |
| Rename | Not implemented. | Rename note, heading, block, tag. |
| Symbols | Not implemented. | Document and workspace. |
| External changes | Re-indexes on save from the editor. No watcher. | obsidian-git rewrites files under the editor. |
| Storage | `.zk/notebook.db` (SQLite) and `.zk/config.toml` inside the vault. obsidian-git would commit the database unless the vault's `.gitignore` is edited. | Nothing in the vault. |

Patching zk would mean a heading and block index, a new resolver, a watcher,
and four new handlers on top of a schema designed for something else. That is
a rewrite of its LSP carried as a fork.

**markdown-oxide** has the right feature list. A trimmed fork would inherit a
large surface (daily-note date parsing, code lenses, a config file, an async
runtime) and a stalled upstream. Removing code until it is small is more work
than writing the small thing.

**Marksman** is the model for scope and temperament. It does not do block
references, tags, or Obsidian's link resolution, and its unresolved-link
diagnostics are wrong for this vault.

**IWE** imposes its own document model.

So: a new server, about the size of Marksman's core, with Obsidian's link
semantics.

## Principles

1. **Never write to disk.** The server reads files and returns edits. Renames
   are a `WorkspaceEdit` the editor applies. Obsidian can have the same vault
   open; the editor stays the only writer on this side.
2. **No index to invalidate.** Each note is parsed into its headings, links,
   block ids and tags. Everything else (backlinks, resolution, diagnostics) is
   computed from that on request. Parsing is a single pass over the lines; a
   vault of tens of thousands of notes resolves in milliseconds.
3. **No configuration.** Nothing is stored in the vault. Behavior follows
   Obsidian's defaults.
4. **Only advertise what works.** Capabilities are limited to what Helix
   25.07 uses: completion, hover, definition, references, rename (with
   prepare), document and workspace symbols, diagnostics, code actions, and
   `workspace/willRenameFiles`. No document links, code lenses, semantic
   tokens or folding: Helix ignores them.

## Model

A **vault** is found from the file being opened: the nearest ancestor
directory containing `.obsidian`, else the nearest containing `.git`, `.jj` or
`.hg`. A file with neither is handled alone, without scanning its directory.
The editor's `rootUri` is not trusted for this, since Helix reports the
working directory when it finds no marker.

In an Obsidian vault every non-hidden file is indexed, as Obsidian does. In a
plain repository `.gitignore` is respected.

The cursor is always on one of:

| Target | Where |
|---|---|
| note | the path part of a link; or anywhere that is not one of the rows below, meaning the note being edited |
| heading | a heading line, or a `#Heading` part of a link |
| block | a `^id` at the end of a block, or a `#^id` part of a link |
| tag | `#tag` in text or in frontmatter `tags` |

Definition, references, hover and rename are all defined on these four
targets, so they agree with each other.

### Unwritten notes

A link to a note that does not exist is not an error. It names a note that has
not been written yet. Such a note:

- produces no diagnostic;
- is offered in `[[` completion, so later links spell it the same way;
- has references (every link that names it) and can be renamed;
- says so on hover, with the number of notes linking to it;
- opens as a new, unsaved buffer on go-to-definition, at the path Obsidian
  would create it. Nothing exists on disk until it is written. A code action
  creates the empty file without leaving the current note.

### Link resolution

Obsidian's rules. Names are case-insensitive. `[[Name]]` matches a note by
file name anywhere in the vault. `[[dir/Name]]` matches by path suffix. With
several candidates the order is: exact path from the vault root, path relative
to the linking note, same folder, shallowest, alphabetical.

Generated links (completion, rename) use the name alone when it is unique in
the vault and the full path from the root otherwise. That is Obsidian's
default "shortest path when possible", so `11/16` is never written for
`2026/11/16`.

Heading comparison ignores case and punctuation, as Obsidian's does.
`#A#B` means heading `B` under heading `A`.

Markdown links `[text](path.md#heading)` take part in everything except
completion when they resolve to a file in the vault.

### Diagnostics

Only things that are broken regardless of intent:

- a heading or block that does not exist in a note that does;
- an embedded attachment (`![[figure.png]]`) that does not exist.

### Rename

Moving files is described once: given a set of moves, every link must resolve
afterward to the file it resolved to before, and links to a moved file are
rewritten in the generated form above. `textDocument/rename` on a note and
`workspace/willRenameFiles` (Helix's `:move`) are both that function.

Helix asks `willRenameFiles` again while applying a rename edit that contains
a file rename. The server remembers the rename it just issued and answers
that echo with nothing.

## Helix specifics

- Helix closes the completion menu on any non-word character and filters the
  list by the last word typed. Note titles are sentences. The server therefore
  filters on the whole text after `[[`, marks results incomplete so Helix asks
  again on each keystroke, registers space as a trigger character, and sets
  each item's filter text to the query followed by the title so Helix's own
  filter keeps the server's order.
- Helix applies edits to files that are not open by opening them as modified
  buffers. After a rename, `:wa` writes them.
- Positions are negotiated as UTF-8 when offered, UTF-16 otherwise.
- Helix has no file watcher for servers. weft watches the vault itself.

## Structure

One thread owns all state and handles requests in order. A watcher thread
turns file system events into messages for it. Open buffers override what is
on disk.

```
src/
  main.rs       entry, argument handling
  server.rs     LSP loop, capabilities, document sync
  vault.rs      files, names, resolution, link text
  parse.rs      line scanner: frontmatter, headings, links, blocks, tags
  text.rs       positions and encodings
  target.rs     what the cursor is on
  complete.rs   hover.rs  refs.rs  rename.rs  symbols.rs  diagnose.rs  action.rs
  watch.rs      file system events
```

Dependencies: `lsp-server` and `lsp-types` (the protocol, without an async
runtime), `notify` (watching), `ignore` (walking), `nucleo-matcher` (the fuzzy
matcher Helix uses, so ranking feels the same), `serde`.

## Not done, on purpose

- Completion for Markdown-style links.
- Generating block ids on completion. Existing `^ids` are completed.
- Daily-note commands, templates, tag hierarchies, graph views.
- Reading Obsidian's settings. Non-default link formats are not generated.
