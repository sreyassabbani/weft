# weft

A language server for folders of Markdown notes that link to each other the
way Obsidian notes do. Written for Helix.

- `[[` completion that keeps working through spaces and punctuation in
  titles, with headings after `#`, block ids after `#^`, frontmatter
  aliases, and notes not written yet
- `@` for daily notes: `@today`, `@fri`, `@11.15` or `@2026.11.15` completes
  to a plain `[[2026/11/15]]`, in whatever layout the vault's daily notes use
- go to definition on links, notes, headings and blocks; on a note not
  written yet it opens a new buffer where Obsidian would create it
- references, hover, document and workspace symbols
- rename notes, headings and block ids; `:move` keeps links working
- warnings only for what is broken: a missing heading or block in a note
  that exists, or a missing embedded file

Links follow Obsidian's rules, and new links are written the way Obsidian
writes them by default: the name alone when it is unique, the full path
otherwise.

weft never writes files. Renames come back to the editor as edits; Helix
opens every changed note as a modified buffer, and `:wa` writes them. If
something else (Obsidian, a git pull) changes those notes before you write,
discard the buffer with `:bc!` and rename again.

It watches the vault itself and keeps nothing in it.

## Change lists

`weft changes <since> <until>` prints the notes created, modified and deleted
in git commits over that range, as the `%% LIST CREATED %%`, `MODIFIED` and
`DELETED` blocks obsidian-list-modified writes into daily notes. It only
reads; a sync script can append its output to the day's note. `--into`
names that note, and `--exclude` leaves out notes matching a
`.gitignore`-style pattern, such as the daily notes themselves.

## Vaults

The vault is the nearest folder marked as one, else the nearest containing
`.git`, `.jj` or `.hg`. A folder is marked by an empty `.weft` file, or by
the `.obsidian` folder Obsidian keeps. In a marked vault every file that is
not hidden is part of it, so attachments resolve by name anywhere; in a
repository, Markdown files not ignored by `.gitignore`. A file outside both
is handled on its own.

## Helix

```toml
[[language]]
name = "markdown"
roots = [".weft", ".obsidian", ".git"]
language-servers = ["weft"]

[language-server.weft]
command = "weft"
```

A new note opened from a link is written with `:w!` when its folder does not
exist yet.

Set `WEFT_LOG=1` in the server's environment to log to Helix's
`:log-open`.

## Building

```sh
cargo build --release
cargo test
WEFT_TEST_VAULT=/path/to/a/copy/of/a/vault cargo test --release --test vault
```

`DESIGN.md` has the details.
