// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! The desktop integration is three files that only work together.
//!
//! A window carries an id, a desktop entry claims that id, and the entry names
//! an icon a theme has to be able to resolve. Each of the three is valid on its
//! own while the chain is broken, and the failure is silent: the application
//! launches, and shows a generic icon. These tests are what makes the chain
//! mechanical rather than remembered.

use std::path::{Path, PathBuf};

use kori_app::theme::APP_ID;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root is two levels above this crate")
}

/// Read one key out of a desktop entry. The format is `key=value` per line,
/// which is all this file uses: no locale suffixes, no second group.
fn entry_value<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    entry.lines().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key).then(|| value.trim())
    })
}

#[test]
fn the_desktop_entry_claims_the_id_the_window_declares() {
    let entry = std::fs::read_to_string(repo_root().join("packaging/desktop/kori.desktop"))
        .expect("packaging/desktop/kori.desktop is committed");

    assert_eq!(
        entry_value(&entry, "StartupWMClass"),
        Some(APP_ID),
        "the entry has to claim the app_id crates/app/src/main.rs sets, \
         or the compositor finds no icon for the window"
    );
}

#[test]
fn the_declared_icon_is_a_file_that_ships() {
    let root = repo_root();
    let entry = std::fs::read_to_string(root.join("packaging/desktop/kori.desktop"))
        .expect("packaging/desktop/kori.desktop is committed");

    let icon = entry_value(&entry, "Icon").expect("the entry names an icon");
    let installed = root.join(format!("packaging/icons/{icon}.svg"));

    assert!(
        installed.is_file(),
        "Icon={icon} resolves to nothing under packaging/icons/, so the install \
         step in docs/from-source.md would put no artwork where the entry \
         looks for it"
    );
}

/// gdk-pixbuf decides a stream's format before it parses it, by matching the
/// head of the file against each loader's signature. The librsvg loader claims
/// a stream whose `<svg` falls inside the first 256 bytes and no further, so an
/// icon carrying its license header above the root element is not an SVG as far
/// as every GTK icon lookup is concerned: `Icon=kori` fails with "couldn't
/// recognize the image file format" and the window shows nothing, while the
/// entry, the app_id and the theme cache are all still correct. Measured on
/// gdk-pixbuf 2.42 with a padded probe: byte 256 loads, byte 257 does not.
const SVG_SIGNATURE_WINDOW: usize = 256;

#[test]
fn the_icon_is_a_format_a_theme_lookup_can_recognize() {
    let root = repo_root();
    let entry = std::fs::read_to_string(root.join("packaging/desktop/kori.desktop"))
        .expect("packaging/desktop/kori.desktop is committed");

    let icon = entry_value(&entry, "Icon").expect("the entry names an icon");
    let artwork = std::fs::read(root.join(format!("packaging/icons/{icon}.svg")))
        .expect("the icon the entry names is committed");

    let offset = artwork
        .windows(4)
        .position(|bytes| bytes == b"<svg")
        .expect("the artwork has a root svg element");

    assert!(
        offset <= SVG_SIGNATURE_WINDOW,
        "<svg starts at byte {offset} of packaging/icons/{icon}.svg, past the \
         {SVG_SIGNATURE_WINDOW}-byte window gdk-pixbuf sniffs; move the header \
         comment inside the root element so the element opens the file"
    );
}

#[test]
fn the_appstream_component_launches_the_entry_that_ships() {
    let root = repo_root();
    let metainfo = std::fs::read_to_string(
        root.join("packaging/desktop/io.github.arthjean.kori.metainfo.xml"),
    )
    .expect("the AppStream component is committed");

    let launchable = metainfo
        .split_once("<launchable type=\"desktop-id\">")
        .and_then(|(_, rest)| rest.split_once("</launchable>"))
        .map(|(id, _)| id.trim().to_owned())
        .expect("the component declares a desktop-id launchable");

    assert!(
        root.join("packaging/desktop").join(&launchable).is_file(),
        "the component launches {launchable}, which is not a file that ships; \
         a store would list the application and fail to start it"
    );
}
