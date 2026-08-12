#!/bin/sh
# SPDX-FileCopyrightText: 2026 Arthur Jean
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Lays out everything a release ships, from binaries already built by
# `cargo build --release`. One script rather than steps inside the workflow, so
# the layout that reaches users can be produced and inspected on a workstation
# with the same command the release runs.
#
#   packaging/stage.sh 0.1.0
#
# Writes:
#   dist/root/                      the filesystem the .deb and .rpm install
#   dist/kori-<version>-x86_64-linux.tar.gz   the same tree, prefix-relative,
#                                             for every distribution without a
#                                             package of its own

set -eu

version=${1:?usage: packaging/stage.sh <version, without the leading v>}
repo=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo"

target=${CARGO_TARGET_DIR:-target}/release
dist=dist

# The version is written in four places and a release where they disagree is a
# package whose software center lists a version the binary does not carry, or a
# changelog describing a release nobody can install. This is the only place that
# can catch it, because each file is individually valid.
manifest_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
if [ "$manifest_version" != "$version" ]; then
    echo "stage: Cargo.toml declares $manifest_version, release is $version" >&2
    exit 1
fi
metainfo=packaging/desktop/io.github.arthjean.kori.metainfo.xml
if ! grep -q "<release version=\"$version\"" "$metainfo"; then
    echo "stage: $metainfo carries no <release version=\"$version\">" >&2
    exit 1
fi
if ! grep -q "^## $version " CHANGELOG.md; then
    echo "stage: CHANGELOG.md carries no '## $version' section" >&2
    exit 1
fi

for binary in kori korid; do
    if [ ! -x "$target/$binary" ]; then
        echo "stage: $target/$binary is missing; run cargo build --release first" >&2
        exit 1
    fi
done

rm -rf "$dist"
root=$dist/root
install -Dm0755 "$target/kori" "$root/usr/bin/kori"
install -Dm0755 "$target/korid" "$root/usr/bin/korid"
install -Dm0644 packaging/udev/70-kori.rules "$root/usr/lib/udev/rules.d/70-kori.rules"
install -Dm0644 packaging/desktop/kori.desktop "$root/usr/share/applications/kori.desktop"
install -Dm0644 packaging/icons/kori.svg "$root/usr/share/icons/hicolor/scalable/apps/kori.svg"
install -Dm0644 "$metainfo" "$root/usr/share/metainfo/io.github.arthjean.kori.metainfo.xml"

# The repository's unit starts the daemon from ~/.local/bin, which is where the
# README's manual install puts it. A packaged unit has to name /usr/bin instead,
# and systemd takes an absolute path there: a unit whose ExecStart points at a
# path the package did not write is a service that fails at every start with no
# indication why. The substitution is asserted rather than assumed, so renaming
# the field in the source unit breaks the release instead of shipping a unit
# that starts nothing.
unit=$root/usr/lib/systemd/user/korid.service
mkdir -p "$(dirname "$unit")"
sed 's|^ExecStart=%h/\.local/bin/korid$|ExecStart=/usr/bin/korid|' \
    packaging/systemd/korid.service >"$unit"
chmod 0644 "$unit"
if ! grep -q '^ExecStart=/usr/bin/korid$' "$unit"; then
    echo "stage: packaging/systemd/korid.service no longer carries the ExecStart line this rewrites" >&2
    exit 1
fi

# The same payload, prefix-relative, for a distribution with no package here.
# install.sh in it takes a PREFIX and rewrites the unit accordingly, so this copy
# keeps the home-directory ExecStart the repository's unit has.
name=kori-$version-x86_64-linux
tree=$dist/tarball/$name
install -Dm0755 "$target/kori" "$tree/bin/kori"
install -Dm0755 "$target/korid" "$tree/bin/korid"
install -Dm0644 packaging/systemd/korid.service "$tree/lib/systemd/user/korid.service"
install -Dm0644 packaging/udev/70-kori.rules "$tree/lib/udev/rules.d/70-kori.rules"
install -Dm0644 packaging/desktop/kori.desktop "$tree/share/applications/kori.desktop"
install -Dm0644 packaging/icons/kori.svg "$tree/share/icons/hicolor/scalable/apps/kori.svg"
install -Dm0644 "$metainfo" "$tree/share/metainfo/io.github.arthjean.kori.metainfo.xml"
install -Dm0644 LICENSE "$tree/LICENSE"
install -Dm0644 README.md "$tree/README.md"
install -Dm0755 packaging/install.sh "$tree/install.sh"

# Same bytes for the same input: a fixed member order, no ownership from the
# build host, and an mtime taken from the commit being released rather than from
# the clock. Two runs of this script on the same commit produce the same archive.
epoch=${SOURCE_DATE_EPOCH:-$(git log -1 --pretty=%ct 2>/dev/null || echo 0)}
tar --create --gzip --file "$dist/$name.tar.gz" \
    --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
    --directory "$dist/tarball" "$name"

echo "stage: $dist/$name.tar.gz"
echo "stage: $root"
