#!/bin/sh
# Add the emulator to the desktop's application menu (freedesktop entry + icon)
# for this checkout. It starts in the checkout so ./roms, ./titles, ./hdf,
# ./adf and ./dir are found. Run it again if the checkout moves.
set -e
here=$(cd "$(dirname "$0")/.." && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
apps="$data/applications"
icons="$data/icons/hicolor/scalable/apps"

[ -x "$here/target/release/amiga" ] || (cd "$here" && cargo build --release)

mkdir -p "$apps" "$icons"
cp "$here/data/amiga-emulator.svg" "$icons/amiga-emulator.svg"
cat > "$apps/amiga-emulator.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Amiga
GenericName=Amiga Emulator
Comment=Run Amiga Forever titles or set up an Amiga
Exec="$here/target/release/amiga" %f
Path=$here
Icon=amiga-emulator
Terminal=false
Categories=Game;Emulator;
Keywords=amiga;commodore;emulator;workbench;kickstart;
StartupWMClass=amiga-emulator
EOF

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$apps" || true
fi
echo "Installed $apps/amiga-emulator.desktop"
