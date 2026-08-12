#!/bin/sh
# SPDX-FileCopyrightText: 2026 Arthur Jean
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Compares what the built binaries link against with what
# packaging/linked-libraries.txt records, and therefore with what the packages
# declare as dependencies.
#
#   packaging/check-linked-libraries.sh target/release
#
# The check is one-directional on purpose. A soname the binaries name and the
# record does not is an undeclared runtime dependency, which is a package that
# installs and then fails to start; that fails the run. A soname the record has
# and the binaries no longer name is only a dependency the packages could drop,
# and glibc merging libm into libc across versions makes it a normal difference
# between build hosts; that is reported and does not fail.

set -eu

dir=${1:?usage: packaging/check-linked-libraries.sh <directory holding kori and korid>}
record=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)/linked-libraries.txt

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for binary in kori korid; do
    objdump -p "$dir/$binary" | awk -v b="$binary" '$1 == "NEEDED" { print b, $2 }'
done | sort >"$work/actual"

grep -v -e '^#' -e '^[[:space:]]*$' "$record" | sort >"$work/recorded"

undeclared=$(comm -23 "$work/actual" "$work/recorded")
if [ -n "$undeclared" ]; then
    echo "check-linked-libraries: these are linked and not recorded in $record:" >&2
    echo "$undeclared" >&2
    echo "Record them there and declare the matching package in packaging/nfpm.yaml." >&2
    exit 1
fi

dropped=$(comm -13 "$work/actual" "$work/recorded")
if [ -n "$dropped" ]; then
    echo "check-linked-libraries: recorded and no longer linked by this build:"
    echo "$dropped"
fi

echo "check-linked-libraries: $(wc -l <"$work/actual") linked libraries, all recorded"
