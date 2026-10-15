# Building RyukNotes on macOS

The app is a Tauri 2 app. There are no prebuilt releases — you build it yourself. Tauri cannot cross-compile, so the .app/.dmg must be built on a Mac.

## Prerequisites

1. Xcode Command Line Tools: `xcode-select --install`
2. Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. Node + pnpm: `brew install node pnpm` (or use fnm/nvm — anything that gets you Node 18+)

## Build

```sh
git clone https://github.com/CallMeBakugo/RyukNotes.git
cd RyukNotes
pnpm install
pnpm tauri build
```

The bundle lands in `src-tauri/target/release/bundle/`:

- `macos/Ryuk's Notebook.app` — drag into /Applications
- `dmg/*.dmg` — installer disk image

## Notes

- Unsigned build: first launch will hit Gatekeeper. Right-click the app, choose Open, then Open again. Or sign ad-hoc with `codesign --force --deep -s - "Ryuk's Notebook.app"`.
- The release build embeds the frontend assets (frontendDist `../build`, produced by `beforeBuildCommand: pnpm build`) — no dev server is needed. If a build you're running ever asks for localhost:5173, it was built wrong; rebuild with `pnpm tauri build`, not `tauri dev` or a copied dev binary.
- Dev mode (hot reload) is `pnpm tauri dev`.
- Your notes vault (`~/Notes/ryuknotes.rn`) is per-machine; copy it over if you want your existing notes on the Mac.