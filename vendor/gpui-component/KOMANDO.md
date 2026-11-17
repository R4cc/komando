# gpui-component 0.9.1, patched for Komando

This is gpui-component 0.9.1 from crates.io, used through `[patch.crates-io]` in Komando's
`Cargo.toml`. `komando.patch` holds the changes as a diff against the published crate. Each
change is marked `Komando:` in the source.

## 1. A menu reports the item it highlights

`PopupMenu::on_highlight` takes a handler that is told whenever the highlighted item changes,
whether the arrow keys or the pointer moved it, with the item's index (or `None` when nothing
is highlighted). The menu keeps its highlight private, and nothing else in the crate says
which item is under the keyboard.

Komando's theme and terminal color menus use it to show each entry while you move through the
list, then put the saved one back if the menu closes without a pick (see
`src/ui/theme_picker.rs`).

The handler runs from `Window::defer` after the menu has drawn, so it can restyle every window
without updating the menu in the middle of its own render.

## 2. Menu icons and checkmarks use a shared 14 px gutter

The popup renderer uses a 14 px slot at the default text scale for its leading
icons, checkmarks and placeholders. Rows with text alone share the same label
alignment when a menu has a leading icon or checkmark.
