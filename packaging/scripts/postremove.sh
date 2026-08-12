#!/bin/sh
# SPDX-FileCopyrightText: 2026 Arthur Jean
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Runs as root after the files are gone, on removal and on the removal half of
# an upgrade.
#
# The kori group is left in place. Deleting it would orphan the group ownership
# on any hwmon attribute the rule already changed, and would silently drop the
# membership of every account that has it, for a reinstall to recreate a group
# with a different id. A stale empty system group costs nothing.

set -eu

if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi

exit 0
