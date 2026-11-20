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
| Rename | Not implemented. | Rename note, heading, block. |
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
2. **Only parses are cached.** Each note is parsed into its headings, links,
   block ids and aliases, and the parse is kept until the file changes (an
   editor change, or a watcher event for a file not open). Everything else
   (backlinks, resolution, diagnostics) is computed from the parses on
   request. Parsing is a single pass over the lines.
3. **No configuration.** Nothing is stored in the vault. Behavior follows
   Obsidian's defaults.
4. **Only advertise what works.** Capabilities are limited to what Helix
   25.07 uses: completion, hover, definition, references, rename (with
   prepare), document and workspace symbols, diagnostics (pushed; 25.07 has
   no pull diagnostics), and `workspace/willRenameFiles`. No
   document links, code lenses, semantic tokens or folding: Helix ignores
   them.

## Model

A **vault** is found from the file being opened: the nearest ancestor
directory marked as a vault (an empty `.weft` file, or Obsidian's `.obsidian`
folder), else the nearest containing `.git`, `.jj` or `.hg`. `.weft` lets a
vault keep working once Obsidian is gone; it holds nothing. A file with neither is handled alone, without scanning its directory.
The editor's `rootUri` is not trusted for this, since Helix reports the
working directory when it finds no marker.

In a marked vault every non-hidden file is indexed, as Obsidian does. In a
plain repository `.gitignore` is respected and only Markdown files are
listed; an embed there is checked by looking for the file at its path. Only
Markdown files are read. The walk finishes before the first request is
answered.

Open buffers win over disk: while a note is open, watcher events for it are
ignored and every range is computed from the buffer. A buffer for a file not
on disk yet is a note like any other.

weft runs on every Markdown file. In a code repository it behaves like
Marksman over that repository's notes.

The cursor is always on one of:

| Target | Where |
|---|---|
| note | the path part of a link; or anywhere that is not one of the rows below, meaning the note being edited. `[[#Heading]]` and `[[#^id]]` name the current note. |
| heading | a heading line, or a `#Heading` part of a link |
| block | a `^id` at the end of a block, or a `#^id` part of a link |

Definition, references, hover and rename are all defined on these three
targets, so they agree with each other. Tags are not a target (see below).

### Unwritten notes

A link to a note that does not exist is not an error. It names a note that has
not been written yet. Such a note:

- produces no diagnostic;
- is offered in `[[` completion, so later links spell it the same way;
- has references (every link that names it) and can be renamed;
- says so on hover, with the number of notes linking to it;
- opens as a new, unsaved buffer on go-to-definition, at the path Obsidian
  would create it: the path in the link, else the vault root. Nothing exists
  on disk until it is written.

### Link resolution

Obsidian's rules. Names are case-insensitive. `[[Name]]` matches a note by
file name anywhere in the vault. `[[dir/Name]]` matches by path suffix. With
several candidates the order is: exact path from the vault root, path relative
to the linking note, same folder, shallowest, alphabetical.

Generated links (completion, rename) use the name alone when it is unique in
the vault and the full path from the root otherwise. That is Obsidian's
default "shortest path when possible", so `11/16` is never written for
`2026/11/16`.

A link may carry `.md` (`[[Note.md]]`). Headings compare case-insensitively
after the characters Obsidian strips from heading links (`# | ^ : % [ ]`) are
turned into spaces and runs of space collapsed; `C` and `C++` stay distinct.
`#A#B` means heading `B` under heading `A`, falling back to any heading `B`.
A heading is only reported missing when nothing matches. A link to a heading
counts as a reference to its note, as in Obsidian's backlinks.

Frontmatter `aliases:` are offered by completion, which inserts
`[[Directional Edges|Neighbour Graphs]]`, and shown on hover. As in Obsidian,
a bare `[[Neighbour Graphs]]` does not resolve to the note.

Markdown links `[text](path)` are not note links: in practice they are web
links. They only answer go-to-definition, when `path` (relative to the note,
percent-decoded, optional `#heading`) names an existing file. This makes `gd`
work on local links in READMEs, where Helix's `gf` guesses at the word under
the cursor.

### Diagnostics

Only things that are broken regardless of intent:

- a heading or block that does not exist in a note that does;
- an embedded attachment (`![[figure.png]]`) that does not exist.

Links inside frontmatter, fenced code, inline code and `$$` math are not
links. Frontmatter is read for `aliases:` (and the older `alias:`) as a
scalar, inline list or block list. `[^1]` is
a footnote, not a block reference.

### Rename

Moving files is described once: given a set of moves, every link must resolve
afterward to the file it resolved to before, and links to a moved file are
rewritten in the generated form above. `textDocument/rename` on a note and
`workspace/willRenameFiles` (Helix's `:move`) are both that function.

Helix asks `willRenameFiles` again while applying a rename edit that contains
a file rename (`Editor::move_path`). The server remembers the exact move it
just issued and answers that one echo with nothing; the memory is cleared by
the echo, by any other request, or after a few seconds.

Safety, since Helix applies edits one by one and its file rename replaces an
existing destination:

- A rename whose destination exists (compared case-insensitively) is refused
  with an error, as is a name containing a character Obsidian forbids.
- Edits to open notes carry the buffer version; edits to closed notes are
  computed from the file as read just before answering.
- Every edited note is opened by Helix as a modified buffer. `:wa` writes
  them; `:bc!` on one discards it. A pull landing between the answer and
  `:wa` can still conflict; that is inherent to editor-applied edits and the
  README says so.

In the edit, text edits come first, addressed to the old paths, and the
`RenameFile` last. A display alias is kept; the alias of a link written as
`[[Old title|Old title]]` is not rewritten.

## Helix specifics

- Helix closes the completion menu on any non-word character (anything but
  alphanumerics and `_`) and filters the list by the word before the cursor.
  Note titles are sentences with quotes and punctuation. The server therefore
  filters on the whole text after `[[`, marks results incomplete so Helix asks
  again on each keystroke, registers space and ASCII punctuation as trigger
  characters (outside a link these answer nothing), and sets each item's
  filter text to the query followed by the title so Helix's own filter keeps
  the server's order. Helix first sorts items by `sortText` (else label), so
  each item carries its rank as `sortText`.
- Workspace symbols are answered as flat `SymbolInformation`; Helix drops
  the nested form.
- Helix applies edits to files that are not open by opening them as modified
  buffers. After a rename, `:wa` writes them.
- Positions are negotiated as UTF-8 when offered, UTF-16 otherwise.
- Helix has no file watcher for servers: it reports only files it writes
  itself. weft watches the vault itself, ignoring hidden folders (`.git`,
  `.obsidian`, `.trash`), coalesces bursts (a pull touches many files) for
  100 ms, re-reads what changed, and republishes diagnostics for every open
  note, including empty lists to clear old ones.
- Logging goes to stderr (Helix's `:log-open`), quiet unless `WEFT_LOG` is
  set.
- Go-to-definition on a note not written yet opens a new buffer at its
  path. `:w!` writes it, creating its folder (`:w` does not create
  folders).

## Structure

One thread owns all state and handles requests in order. A watcher thread
turns file system events into messages for it. Open buffers override what is
on disk.

```
src/
  main.rs       entry, `--version`
  server.rs     LSP loop, capabilities, document sync
  vault.rs      files, names, resolution, link text
  parse.rs      line scanner: frontmatter aliases, headings, links, blocks
  text.rs       positions and encodings
  target.rs     what the cursor is on
  complete.rs   hover.rs  refs.rs  rename.rs  symbols.rs  diagnose.rs
```

Dependencies: `lsp-server` and `lsp-types` (the protocol, without an async
runtime), `crossbeam-channel` (select over editor and watcher),
`notify` (watching), `ignore` (walking), `nucleo-matcher` (the fuzzy matcher
Helix uses, so ranking feels the same), `serde_json`, `percent-encoding`.
Frontmatter is read by hand for `aliases:` only; no YAML parser.

## Not done, on purpose

- Tags. The vault has none; they can come later as a fourth target.
- Markdown links beyond go-to-definition: no completion, references or
  rename.
- Generating block ids. Existing `^ids` are completed.
- A code action to create a note. `gd` already opens the new buffer, and
  Helix's `CreateFile` would truncate a file created meanwhile.
- A hint on links to notes not written yet. `end-of-line-diagnostics` would
  print one on every such line; hover says it instead.
- Daily-note commands, templates, graph views.
- Reading Obsidian's settings. Non-default link formats are not generated.

## Plan

Built in this order, each step committed with its tests:

1. `text`, `parse`: positions in UTF-8/UTF-16, the line scanner. Unit tests
   for every link form in the vault (aliases, `#A#B`, `\|` in tables, embeds
   with `|313` sizes, titles with quotes, `[^1]`, code and math).
2. `vault`: discovery of the root, walking, name table, resolution, link text
   generation. Tests for duplicate daily-note basenames, `[[2.71]]`,
   `atlas.design/` folders, aliases.
3. `server`: initialize (encoding, capabilities), document sync
   (incremental), the select loop, `watch`.
4. Handlers: definition, hover (note: path, aliases, backlink count and the
   opening lines or the heading's section; unwritten: "not written yet ·
   linked from N notes"), references, completion, symbols, diagnostics,
   rename and `willRenameFiles`.
5. README, `flake.nix`-free packaging in `~/config` (`pkgs/weft.nix` built
   with `rustPlatform.buildRustPackage` from a `git+file` input, so `target/`
   is never copied), the Helix language entry.

Tests:

- Unit tests per module; protocol tests drive the server through an
  in-memory `lsp-server` connection over a fixture vault.
- A fixture vault with expected targets for each link: colliding basenames,
  aliases against real titles, `[[#Heading]]`, block ids, `C` against `C++`,
  `[[Note.md]]`.
- Vault tests, run when `WEFT_TEST_VAULT` points at a copy of a real vault:
  every note parses; a spot-check list of links resolves to the exact files
  and headings Obsidian shows; for every note and every heading, a simulated
  rename, applied to the vault, leaves every wikilink (including links from
  the moved note and links shadowed by the new name) resolving to the same
  file and heading as before.
- By hand in Helix 25.07.1, against the copy: completion on multi-word
  titles with punctuation, `gd` on a note link, a heading link and an
  unwritten note, `gr`, rename of a note and of a heading, hover.
