#!/bin/sh
# Run from a terminal beside the downloaded ryuknotes and ryuk binaries.
set -eu
export LC_ALL=C
bundle_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
app="$bundle_dir/ryuknotes"
cli="$bundle_dir/ryuk"

if [ "$(uname -s)" != Linux ]; then
    echo "This installer is for Linux." >&2
    exit 1
fi
for binary in "$app" "$cli"; do
    if [ ! -f "$binary" ]; then
        echo "Missing $binary. Extract the complete Linux archive first." >&2
        exit 1
    fi
done
if ! command -v ldd >/dev/null 2>&1; then
    echo "Cannot check shared libraries: ldd is missing. Install your distro's libc tools, or use the .deb/.rpm package." >&2
    exit 1
fi

# Package names differ across distros. Never execute a command supplied by a download.
ID= ID_LIKE=
if [ -r /etc/os-release ]; then . /etc/os-release; fi
case " $ID $ID_LIKE " in
    *debian*|*ubuntu*)
        manager=apt
        packages='libwebkit2gtk-4.1-0 libgtk-3-0 libayatana-appindicator3-1 librsvg2-2 xdg-utils'
        ;;
    *fedora*|*rhel*)
        manager=dnf
        packages='webkit2gtk4.1 gtk3 libayatana-appindicator-gtk3 librsvg2 xdg-utils'
        ;;
    *arch*)
        manager=pacman
        packages='webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg xdg-utils'
        ;;
    *) manager=; packages= ;;
esac

check_libraries() {
    failed=0
    for binary in "$app" "$cli"; do
        if output=$(ldd "$binary" 2>&1); then
            case "$output" in
                *'not found'*) printf '%s\n%s\n' "$binary:" "$output" >&2; failed=1 ;;
            esac
        else
            printf 'Cannot inspect %s (wrong architecture or unsupported executable?):\n%s\n' "$binary" "$output" >&2
            failed=1
        fi
    done
    return "$failed"
}

echo "Ryuk's Notebook needs WebKitGTK 4.1, GTK 3, an Ayatana tray indicator, librsvg, and xdg-utils."
if ! check_libraries; then
    echo "Required libraries are missing or these binaries cannot run on this system." >&2
fi
# Offer the full set even when ldd passes: tray libraries are loaded dynamically.
if [ -n "$manager" ]; then
    case "$manager" in
        apt) install_command="sudo apt install $packages" ;;
        dnf) install_command="sudo dnf install $packages" ;;
        pacman) install_command="sudo pacman -S --needed $packages" ;;
    esac
    printf '\nDependency command: %s\n' "$install_command"
    printf 'Run this command now? Your package manager will ask before installing. [y/N] '
    answer=
    read -r answer || true
    case "$answer" in
        y|Y|yes|YES)
            case "$manager" in
                apt) sudo apt install $packages ;;
                dnf) sudo dnf install $packages ;;
                pacman) sudo pacman -S --needed $packages ;;
            esac
            ;;
    esac
else
    echo "No automatic dependency command for this distro. Use its package manager to install the libraries listed above."
fi
if ! check_libraries; then
    echo "Installation stopped. Install the missing libraries, then run this installer again." >&2
    exit 1
fi
destination="${XDG_BIN_HOME:-$HOME/.local/bin}"
printf '\nInstall ryuknotes and ryuk into %s? Close the app first. [y/N] ' "$destination"
answer=
read -r answer || true
case "$answer" in
    y|Y|yes|YES)
        mkdir -p "$destination"
        install -m 755 "$app" "$destination/ryuknotes"
        install -m 755 "$cli" "$destination/ryuk"
        echo "Installed. Run $destination/ryuknotes to open Ryuk's Notebook."
        ;;
    *) echo "No binaries installed." ;;
esac
