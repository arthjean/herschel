#!/bin/sh
# SPDX-FileCopyrightText: 2026 Arthur Jean
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Runs as root from the .deb and the .rpm, on install and on upgrade. Everything
# it does is idempotent, and nothing it does is a hardware write.
#
# What it deliberately does not do: add anyone to the kori group, and enable the
# user unit. The first is an account change the operator should make knowingly,
# the second cannot be done from a root hook at all, because `systemctl --user`
# addresses a session that does not exist while the package manager is running.

set -eu

# The group the udev rule hands the hwmon attributes to. Without it, the rule's
# chgrp fails and the attributes stay root-owned, which is the read-only
# direction and the safe one.
if ! getent group kori >/dev/null 2>&1; then
    groupadd --system kori
fi

# Apply the rule now rather than at the next boot. The trigger is limited to the
# two subsystems the rule names, and re-running a rule is a permission change on
# attributes, not a device command.
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules || true
    udevadm trigger --action=change --subsystem-match=hwmon || true
    udevadm trigger --action=change --subsystem-match=hidraw || true
fi

# Caches some desktops read instead of the directory. Both are optional: without
# them the entry appears at the next login.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi

cat <<'EOF'

kori is installed. Two steps are left, and both belong to the user rather than
to this package:

    sudo usermod --append --groups kori <your user>
    systemctl --user enable --now korid.service

Log out and back in after the group change: group membership is read when the
session starts. Until then the daemon runs read-only and says so, rather than
failing silently.

EOF

exit 0
