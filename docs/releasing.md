<!--
SPDX-FileCopyrightText: 2026 Arthur Jean
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Releasing

What a release is, in what order it happens, and what is deliberately not
automated.

## What ships

One tag produces four files, all x86_64:

| Artifact | Reaches |
|---|---|
| `kori_<version>-1_amd64.deb` | Debian, Ubuntu, Mint, Pop!\_OS, elementary |
| `kori-<version>-1.x86_64.rpm` | Fedora, RHEL and rebuilds, openSUSE, Mageia |
| `kori-<version>-x86_64-linux.tar.gz` | everything else, including a machine where no package should be installed at all |
| `SHA256SUMS` | the digests the attestation is issued over |

Arch is served by `packaging/aur/PKGBUILD`, which consumes the tarball above.
The `.deb` and the `.rpm` both come from `packaging/nfpm.yaml`, and everything
in them comes from `dist/root`, which `packaging/stage.sh` writes.

The build runs on `ubuntu-22.04` so the binaries carry glibc 2.35, which is the
oldest GitHub still hosts. That is the floor a user has to clear: Ubuntu 22.04,
Debian 12, Fedora 37, and anything newer.

## What the release proves before it publishes

`release.yml` is three stages: build, install, publish. Nothing reaches the
releases page until every package in it has been installed in a clean container:

| Container | What it settles |
|---|---|
| `ubuntu:22.04`, `debian:12` | apt resolves the `Depends` line from the distribution's own archive, so a package name that exists on only one of them fails here |
| `fedora:41`, `opensuse/tumbleweed` | the same `.rpm` installs on both, which is what the soname `Requires` in `packaging/nfpm.yaml` exist for: `libxkbcommon` on Fedora is `libxkbcommon0` on openSUSE, and depending on the soname is what makes one file serve both |
| `archlinux:base` | the tarball's installer, and the dependency list `packaging/aur/PKGBUILD` declares |

Each of them then asserts the layout, that the postinstall created the `kori`
group, that no binary reports a missing library, that the two libraries the
renderer opens with `dlopen` are actually in the loader cache, and that `korid`
runs. The deb job also removes the package and checks the group survives.

That last one is not decoration: `ldd` catches exactly the failure that made
this build run on `ubuntu-22.04`. A binary linked on a newer host installs
cleanly and then reports `version 'GLIBC_2.39' not found` at the first launch,
with the package manager reporting success throughout.

## Cutting one

1. Bump `version` in `Cargo.toml` and add the matching `<release>` entry to
   `packaging/desktop/io.github.arthjean.kori.metainfo.xml`. `stage.sh` refuses
   to run when either disagrees with the tag, because each file is valid on its
   own while the set is wrong, and the result is a software centre listing a
   version the binary does not carry.
2. Run the four validation commands from `AGENTS.md`.
3. Try the packaging without publishing anything: run the **Release** workflow
   from the Actions tab. It builds, packages, signs and installs, and skips only
   the publication, leaving the artifacts on the run.
4. Tag and push:

   ```bash
   git tag -a v0.1.0 -m "kori 0.1.0"
   git push origin v0.1.0
   ```

   The workflow builds, packages, signs and publishes the release against that
   tag.
5. Update the AUR package, below.

## Verification a user can run

Every artifact is covered by a Sigstore signature over a SLSA build provenance
statement, issued by GitHub's OIDC identity for this workflow. No key is
distributed by this project, and none is held by it:

```bash
gh attestation verify kori_0.1.0-1_amd64.deb --repo arthjean/kori
```

`SHA256SUMS` covers the same files for anyone without GitHub's CLI. There is no
GPG key and no signed apt or dnf repository: those exist to make a repository's
metadata trustworthy, and this project publishes files rather than a repository.
A distribution that later carries Kori signs its own rebuild with its own key,
which is the correct direction.

## The AUR package

One-time, and manual on purpose: an SSH key that can push to the AUR would be a
credential in this repository that mutates something outside it.

```bash
# once: an AUR account with your SSH key, then
git clone ssh://aur@aur.archlinux.org/kori-bin.git
```

At each release, from the clone:

```bash
cp /path/to/kori/packaging/aur/PKGBUILD /path/to/kori/packaging/aur/kori.install .
updpkgsums                       # downloads the published tarball, writes its digest
makepkg --printsrcinfo > .SRCINFO
makepkg --install                # build it and install it before pushing it
git commit -am "kori-bin 0.1.0" && git push
```

`packaging/aur/PKGBUILD` in this repository is the source of truth; the AUR
repository is a copy plus the generated `.SRCINFO`.

## What is not packaged, and why

**Flatpak and Snap.** The daemon writes to `hwmon` attributes under `/sys`, and
a Flatpak sandbox mounts `/sys` read-only with no permission that lifts it
([flatpak#3291](https://github.com/flatpak/flatpak/issues/3291)). A sandboxed
build would install, start, show telemetry, and silently fail every cooling
write. The udev rule is also a root-owned file outside any sandbox, so even a
GUI-only Flatpak would still need the daemon installed by other means: two
install routes for one product, one of which cannot do the job.

**arm64.** Nothing prevents it, and the matrix is a few lines. It stays out
until somebody reports one of these coolers on an arm64 machine, rather than
doubling every release for a configuration this project has never seen.

**A signed apt or dnf repository.** See the verification section: the artifacts
are signed, the repository is what does not exist.

## Adding a distribution channel

Fedora's COPR and openSUSE's Build Service both build from a spec file and sign
with their own key, which is a better trust story than a `.rpm` on a release
page. Neither is set up. Both would consume the same `dist/root` layout, so the
work is a spec file and an account, not a change to what the release produces.
