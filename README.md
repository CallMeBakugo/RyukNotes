<p align="center">
  <img src="docs/icon.png" width="256" alt="Ryuk's Notebook — the Shinigami's notebook" />
</p>

# Ryuk's Notebook

A local-first notes app with the agent built in.

Ryuk's Notebook (the app, binary, and repo are named RyukNotes) is a fork of
[HelixNotes](https://gitlab.com/ArkHost/HelixNotes) (AGPL-3.0), rebuilt around
a single SQLite vault and a companion CLI for AI-agent workflows.

**Status: this is a new project.** It works, but expect rough edges —
bugs are being worked out over the next few weeks.

## What it is

- Notes live in one SQLite database file (a `.rn` vault), not a folder of
  markdown files
- Markdown editor, tags, daily notes, tasks, version history, full-text
  search
- Attachments (images, PDFs, recovery files) stored inside the database,
  deduplicated by content hash
- Linux desktop, self-hosted, no accounts, no cloud, no telemetry

## The agent side

The `ryuk` CLI gives scripted read/write access to the same vault:

```sh
ryuk notes list
ryuk notes search "mullvad"
ryuk notes new "Weekly review" --tag work --stdin
ryuk session restore <hermes-session-id>
ryuk session start --notes <note-id>
```

It is built for the [Hermes Agent](https://hermes-agent.nousresearch.com)
workflow: sessions can be summarized into the vault as notes, and restored
from them later with one command (or one click in the app). Any agent that
can run a CLI can do the same.

## Installing on Linux

Use the `.deb` with `sudo apt install ./<downloaded-file>.deb`, or the `.rpm`
with `sudo dnf install ./<downloaded-file>.rpm`, so your package manager checks
and installs required libraries. For the standalone Linux archive, extract it
and run `sh install.sh` from a terminal for a dependency check and install prompts.
See [Linux installation and runtime requirements](docs/linux-install.md).

## Building from source

Requirements: Rust, Node.js, pnpm, and the Linux development libraries listed
in the [Linux guide](docs/linux-install.md#building-from-source).

```sh
pnpm install
pnpm tauri dev      # development
pnpm tauri build    # release build (app + ryuk CLI)
```

The `ryuk` CLI binary lands in `src-tauri/target/release/ryuk`.

## Credits and license

Forked from HelixNotes by Yuri Karamian (ArkHost) — the editor, UI, and
original architecture are his work. Rebadged and reworked as RyukNotes
under the same AGPL-3.0 license. Upstream: https://gitlab.com/ArkHost/HelixNotes
