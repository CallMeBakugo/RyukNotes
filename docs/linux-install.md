# Installing Ryuk's Notebook on Linux

Downloading a file does not run a dependency check. Install through your package
manager, or run the standalone archive's installer from a terminal before opening
the app. The operating system loads shared libraries before app code runs, so a
missing WebKitGTK library cannot be reported by an in-app dialog.

## Native packages (preferred)

On Debian/Ubuntu, open a terminal in the download directory and use
`sudo apt install ./<downloaded-file>.deb` (replace the filename). Use `apt`
rather than `dpkg -i`: apt resolves dependencies and asks for approval.

On Fedora, use `sudo dnf install ./<downloaded-file>.rpm`. Dnf resolves dependencies
and asks for approval. RPM library requirements also let other RPM package managers
resolve distro-specific library package names. Unsupported distro versions may
need an OS upgrade; do not substitute WebKitGTK 4.0 for 4.1.

## Standalone archive

Extract the complete `ryuknotes_<version>_linux_x86_64.tar.gz` archive. From its directory,
run `sh install.sh` in a terminal. It inspects both binaries with `ldd`, prints
missing libraries or architecture errors, and offers dependency installation on
Debian/Ubuntu, Fedora, and Arch families. Every installation requires your answer;
the package manager handles privilege elevation and its own confirmation. Other
distros get the library requirements for manual installation.

The installer offers to copy `ryuknotes` and `ryuk` into `~/.local/bin` after
the library check passes. Close any running copy first. Ensure that directory is
on your PATH. Keep the archive to rerun the check after moving to another distro.
Only use `ldd` on binaries from a source you trust.

## Runtime requirements

| Purpose | Debian/Ubuntu | Fedora | Arch |
| --- | --- | --- | --- |
| Webview (includes JavaScriptCore and libsoup dependencies) | libwebkit2gtk-4.1-0 | webkit2gtk4.1 | webkit2gtk-4.1 |
| Desktop UI | libgtk-3-0 | gtk3 | gtk3 |
| Tray (loaded dynamically) | libayatana-appindicator3-1 | libayatana-appindicator-gtk3 | libayatana-appindicator |
| SVG icons | librsvg2-2 | librsvg2 | librsvg |
| Open links/files in desktop apps | xdg-utils | xdg-utils | xdg-utils |

Your package manager installs the libraries' transitive dependencies (GLib, graphics,
font, accessibility, multimedia and other platform libraries). Exact libc/libstdc++
requirements depend on the release build; native package metadata and the standalone
installer check these too. A graphical desktop session is required for the GUI.
The CLI does not need a webview, but the archive installer checks the whole bundle.

SQLite is compiled into RyukNotes and HTTPS uses Rustls: separate SQLite and OpenSSL
runtime packages are not required by those features. Hermes and a terminal emulator
are needed only for the optional Hermes workflow. AppImages are not produced by the
Linux release job; FUSE is not a requirement for these downloads.

## Building from source

In addition to Rust, Node.js and pnpm, Linux needs development packages. The CI
build uses `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`
and `librsvg2-bin` on Ubuntu, plus normal C/C++ build tools and pkg-config. These
development packages are not needed merely to run a downloaded release.

## If opening still fails

Run `ryuknotes` in a terminal and keep the complete error. “Error while loading
shared libraries” identifies an unmet runtime library; rerun the installer or
reinstall the native package through the package manager. “GLIBC… not found” or
“GLIBCXX… not found” means this build needs a newer platform library: use a supported
newer distro or build from source on your distro. Do not copy system libraries
from another distribution.
