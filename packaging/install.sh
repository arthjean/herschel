#!/bin/sh
# SPDX-FileCopyrightText: 2026 Arthur Jean
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Installs the release tarball on a distribution that has no package here. Run
# it from the directory the archive unpacked into:
#
#   ./install.sh                 installs under ~/.local
#   PREFIX=/opt/kori ./install.sh
#
# It never asks for root and never runs sudo itself. The one step that needs
# root is the udev rule, and it is printed at the end for the operator to run
# and read first: it is the file that decides which two devices this software is
# allowed to touch.

set -eu

prefix=${PREFIX:-$HOME/.local}
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$here"

install -Dm0755 bin/kori "$prefix/bin/kori"
install -Dm0755 bin/korid "$prefix/bin/korid"

install -Dm0644 share/applications/kori.desktop "$prefix/share/applications/kori.desktop"
install -Dm0644 share/icons/hicolor/scalable/apps/kori.svg \
    "$prefix/share/icons/hicolor/scalable/apps/kori.svg"
install -Dm0644 share/metainfo/io.github.arthjean.kori.metainfo.xml \
    "$prefix/share/metainfo/io.github.arthjean.kori.metainfo.xml"

# systemd takes an absolute path in ExecStart, so the unit is written against
# the prefix this run installed into rather than shipped with one guessed.
unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/korid.service
mkdir -p "$(dirname "$unit")"
sed "s|^ExecStart=.*|ExecStart=$prefix/bin/korid|" lib/systemd/user/korid.service >"$unit"
chmod 0644 "$unit"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$prefix/share/applications" || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf "$prefix/share/icons/hicolor" || true
fi

cat <<EOF

Installed under $prefix. Three steps are left.

1. The udev rule, which grants access to exactly two devices, 1e71:300e and
   1e71:2021, and to nothing else. Read $here/lib/udev/rules.d/70-kori.rules
   before running this:

       sudo groupadd --system kori
       sudo usermod --append --groups kori "\$USER"
       sudo install -m 0644 $here/lib/udev/rules.d/70-kori.rules /etc/udev/rules.d/
       sudo udevadm control --reload
       sudo udevadm trigger --action=change --subsystem-match=hwmon

2. Log out and back in. Group membership is read when the session starts, so
   until then the daemon runs read-only and reports why.

3. Start the daemon with your session:

       systemctl --user daemon-reload
       systemctl --user enable --now korid.service

If $prefix/bin is not on your PATH, add it, or launch the interface from your
desktop: the entry this installed points at the binary directly.

EOF
