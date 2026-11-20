# Notes workflow without Obsidian (plan)

Status: draft, open for discussion. Nothing here is built yet.

The goal is to delete Obsidian entirely. weft today is a language server. To
get there, a few more things have to exist somewhere: the git sync that
obsidian-git does, the per-day change list that obsidian-list-modified
writes, the daily-note command, and a way for weft to recognize the vault
once `.obsidian/` is gone. This plan decides where each belongs, so the
result is a few pieces with clear jobs and not a pile of scripts.

## What Obsidian does now

| Plugin | Behavior relied on |
|---|---|
| obsidian-git | Every 30 min: commit all changes as `vault backup: <date>`, with the changed files in the body. Pull (merge) before push, then push. Pull when Obsidian starts. Commits are signed. Runs only while Obsidian is open. |
| obsidian-list-modified | Keeps three blocks in the daily note, `LIST CREATED`, `LIST MODIFIED`, `LIST DELETED`, each a list of `[[links]]` to notes touched that day. Attachments and `.obsidian/` are excluded. |
| daily notes | `⌘D` opens `YYYY/MM/DD.md` at the vault root, creating it empty. Done: `space d` in Helix, from `.helix/config.toml` in the vault. |
| pasting images | Saves to `attachments/Pasted image <timestamp>.png` and embeds it. Done: `space i`, a vault script reading the clipboard. |
| saving | Saves as you type. Done: Helix auto-save, set in the vault only. |

Given up with Obsidian, on purpose: the graph, Bases (`.base` files stay but
nothing reads them), rendered images (open them with Quick Look), tab groups,
tags, canvas, themes and CSS, note composer, copy-path and delete-file keys.

The graph in particular is not coming back in weft. It is a picture of the
notes more than a way to work with them, it costs screen space, and it is the
wrong model for what these notes are. The test for any feature here is
whether it helps thinking and writing, not whether it shows the vault off.

## Principle: each piece lives where its reason to change lives

1. **The vault** holds what belongs to the notes and should travel with them:
   the notes, `.helix/config.toml` (vault-only keys), `.gitignore`.
2. **weft** holds anything that needs to understand a vault: how links
   resolve, how a link to a note should be written, what counts as a note.
   It is tested, versioned and public, so it holds no personal paths or
   policy.
3. **`~/config`** holds machine policy: which binary is installed, how Helix
   is wired, when sync runs, where the vault is. One module, so that turning
   the notes setup on, off, or onto a new machine is one place.

If a piece needs to know Obsidian's link rules, it goes in weft. If it is a
schedule, a path or a preference, it goes in `~/config`. If it is about these
particular notes, it goes in the vault.

## Proposed shape

### weft: a CLI around the server

`weft` with no arguments (or `weft lsp`) stays the language server, so the
Helix config does not change. Two subcommands are added, both read-only:

- **`weft changes <since> [<until>]`**: notes created, modified and deleted in
  a range of git history, printed as `[[links]]` written the way the server
  writes them (bare name when unique, full path otherwise). Attachments and
  hidden folders are excluded. This is the one part of list-modified that
  needs the vault model, which is why it belongs here.
- **`weft today`**: prints the path of today's daily note. Lets the Helix key
  become `:open %sh{weft today}`, without the `%%`-escaped `date` call, and
  gives one definition of the daily-note path that other tools can reuse.

weft still never writes to the vault. Anything that writes is a caller's
decision.

### weft: recognizing the vault without `.obsidian/`

weft finds a vault by the nearest `.obsidian/`. Without one it treats the
folder as a plain repository, where only Markdown files count. That breaks
this vault: the 25 `![[...png]]` embeds live in `attachments/` and would all
show as missing, and attachment completion would stop.

Proposal: an empty `.weft` file at the vault root marks it, and means the same
as `.obsidian/` does today (every non-hidden file is part of the vault,
attachments resolve by name anywhere). `.obsidian/` keeps working for other
people's vaults. The Helix `roots` entry becomes `[".weft", ".obsidian",
".git"]`. This is the only thing weft would ever ask to have in a vault, and
it is empty.

### `~/config`: one `home/weft.nix` module

Moves the existing pieces together and adds sync:

- **`weft` on `PATH`.** It isn't now: Helix runs it by its Nix store path,
  so `weft` in a terminal or a `%sh{}` finds nothing. The module adds it to
  `home.packages`, which `weft today`, `weft changes` and the sync job need.
- the Helix language entry for Markdown (now in `home/helix.nix`)
- the vault path, set once
- **`launchd.agents.weft-sync`**, described next. `home/anki-blue.nix` and
  `home/hammerspoon.nix` already use `launchd.agents`, so this follows an
  existing pattern.

### Sync, as one launchd job

A small `writeShellApplication` with two triggers:

- **At login:** pull. This is when remote changes are most likely to be
  waiting, and no note is open yet.
- **Every 30 min:** if the tree is dirty, commit `vault backup: <date>` with
  the changed files in the body. Then push. Pull only if the push is
  rejected, meaning GitHub has a commit this Mac doesn't. Then merge and
  push again.

The vault is edited almost only on this Mac (an occasional fix through the
GitHub app), so a mid-session pull should almost never happen. That avoids
the main risk without any detection of open editors: Helix does not reload
files changed on disk, so a pull under an open note leaves a stale buffer,
and writing it can overwrite the pulled version. On a merge conflict the job
stops and leaves it for you.

Sync stays a script in `~/config` and not a weft subcommand: it is git
policy (intervals, signing, merge strategy), needs no vault model, and would
make a public tool carry one person's backup habits.

### The vault

`.helix/config.toml` keeps `space d`, `space i` and auto-save; `space d`
switches to `weft today` once that exists. `.weft` is added; `.obsidian/` is deleted.

## Daily change lists (decided: keep writing them)

Each daily note ends with lists of the notes created, modified and deleted
that day, as `[[links]]`. Those links also make the days you worked on a note
show up when you run `gr` on it.

The sync job, on its first run each day, writes the lists for every finished
day since the last run into that day's note, using `weft changes`, creating
the note if it does not exist (as list-modified does). Only finished days are
written, so a note you have open is never touched. The difference from
list-modified: today's lists appear tomorrow, not live.

The alternative considered was to stop writing lists and have weft show a
note's edit dates on hover, from git, so nothing would be generated into the
notes. Rejected: the lists are wanted in the notes themselves.

## Risks to check before building

- **Signing from launchd.** Commits are signed with GPG. obsidian-git signs
  fine because it runs inside a GUI app with gpg-agent's pinentry available.
  A launchd job may not get a passphrase prompt and would fail to commit.
  Test before relying on it.
- **The switch-over.** obsidian-git and the job must never both run. Order:
  stop using Obsidian, load the job, confirm one commit and push, then
  delete Obsidian and `.obsidian/`.

## Order of work

1. weft: the `.weft` marker. Small, and needed before `.obsidian/` can go.
2. `~/config`: `home/weft.nix`, moving the package and Helix pieces in and
   putting `weft` on `PATH`. No change in behavior.
3. `~/config`: the sync job, tested with signing.
4. Switch over: add `.weft`, quit Obsidian, run on the job for a few days,
   then delete Obsidian and `.obsidian/`.
5. weft: `weft today` and `weft changes`; the sync job writes the lists.

## Open questions

1. Is the plan document's home right? It sits in weft's public repo because
   most of what it proposes is weft's.
