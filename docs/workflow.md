# Notes workflow without Obsidian (plan)

Status: draft, open for discussion. Nothing here is built yet.

weft today is a language server. To retire Obsidian, three more things have to
exist somewhere: the git sync that obsidian-git does, the per-day change list
that obsidian-list-modified writes, and the daily-note command. This plan asks
where each belongs, so the result is a few pieces with clear jobs and not a
pile of scripts.

## What Obsidian does now

| Plugin | Behavior relied on |
|---|---|
| obsidian-git | Every 30 min: commit all changes as `vault backup: <date>`, with the changed files in the body. Pull (merge) before push, then push. Pull when Obsidian starts. Commits are signed. Runs only while Obsidian is open. |
| obsidian-list-modified | Keeps three blocks in the daily note, `LIST CREATED`, `LIST MODIFIED`, `LIST DELETED`, each a list of `[[links]]` to notes touched that day. Attachments and `.obsidian/` are excluded. |
| daily notes | `⌘D` opens `YYYY/MM/DD.md` at the vault root, creating it empty. Now `space D` in Helix, from `.helix/config.toml` in the vault. |

Not covered by this plan, and still a reason to open Obsidian: images, the
graph, Bases, Shiki-rendered code.

## Principle: each piece lives where its reason to change lives

1. **The vault** holds what belongs to the notes and should travel with them:
   the notes, `.helix/config.toml` (vault-only keys), `.gitignore`.
2. **weft** holds anything that needs to understand a vault: how links
   resolve, how a link to a note should be written, what counts as a note.
   It is tested, versioned, and public, so it holds no personal paths or
   policy.
3. **`~/config`** holds machine policy: which binary is installed, how Helix
   is wired, when sync runs, where the vault is. One module, so that turning
   the notes setup on, off, or onto a new machine is one place.

The rule that falls out: if a piece needs to know Obsidian's link rules, it
goes in weft. If it is a schedule, a path, or a preference, it goes in
`~/config`. If it is about these particular notes, it goes in the vault.

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

### `~/config`: one `home/notes.nix` module

Moves the existing pieces together and adds sync:

- the `weft` package and its Helix language entry (now in `home/helix.nix`)
- the vault path, set once
- **`launchd.agents.notes-sync`**: a small `writeShellApplication` run every
  30 min and at login. It does what obsidian-git did: if the tree is dirty,
  commit `vault backup: <date>` with the changed files in the body; pull
  with merge; push. It stops without retrying on a merge conflict and leaves
  it for you. `home/anki-blue.nix` and `home/hammerspoon.nix` already use
  `launchd.agents`, so this follows an existing pattern.

Sync is a script, not a weft subcommand: it is git policy (intervals,
signing, merge strategy), needs no vault model, and would make a public tool
carry one person's backup habits.

### The vault: unchanged, apart from what it already has

`.helix/config.toml` keeps `space D`. If `weft today` lands, the key switches
to it.

## The decision that shapes the rest: change lists written or derived

list-modified's real job is tying ideas to time. There are two ways to keep
that.

**A. Keep writing them into daily notes.** The sync script, at the first run
of each day, writes yesterday's lists into yesterday's note, using
`weft changes`. Writing only finished days avoids clobbering a note you have
open. Old and new daily notes look the same, and `gr` on any note shows the
days it was touched, because those days link to it.

**B. Stop writing them and derive from git.** Commits already record which
notes changed. weft shows it on demand, for example a hover line "edited
Nov 19, Nov 14, Oct 2". No generated text lands in your notes, and nothing
can conflict. But `gr` no longer lists the days, and the record lives in git
rather than in the notes.

A is closer to what you have. B is cleaner but changes how you would see
your history. The existing lists stay either way.

## Risks to check before building

- **Signing from launchd.** Commits are signed with GPG. obsidian-git signs
  fine because it runs inside a GUI app. A launchd job needs gpg-agent to
  have the key unlocked, or the commit fails. Test before relying on it.
- **Helix does not reload files changed on disk.** A pull while a note is
  open leaves a stale buffer, and writing it can overwrite the pulled
  version.
  Options: pull only at login and before push, never mid-session (smallest);
  or have the sync script skip the pull when Helix has the vault open.
  `:reload-all` is the manual fix.
- **Sync with Obsidian still installed.** Run either obsidian-git or the
  launchd job, never both. The switch is: turn off obsidian-git's auto
  commit, push and pull, then load the agent.
- **Phone.** If the vault is edited on a phone through Obsidian mobile, git
  sync there is its own problem and is out of scope.

## Order of work

1. `home/notes.nix`: move the weft and Helix pieces in, with no change in
   behavior.
2. The sync agent, tested with signing, alongside obsidian-git with its
   automatic actions turned off.
3. `weft changes` and `weft today`, then the change-list decision (A or B).
4. Live without Obsidian for a week; uninstall when nothing pulls you back.

## Open questions

1. Change lists: A (written into daily notes) or B (derived from git)?
2. Do you edit the vault on your phone?
3. Should sync pull mid-session, or only at login and right before pushing?
4. Is the plan document's home right? It sits in weft's public repo because
   most of what it proposes is weft's.
