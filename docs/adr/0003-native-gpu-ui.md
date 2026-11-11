# Native GPU UI with gpui and alacritty_terminal

The UI is built in Rust with gpui (Zed's GPU UI framework) plus gpui-component. Terminal emulation uses the `alacritty_terminal` crate. Zed already runs this combination, Windows included. We rejected a web-based UI (Tauri + xterm.js, like Tabby's Electron + xterm.js) even though it would make a polished UI easier: every byte would cross into a JavaScript renderer, which contradicts the speed priority. gpui's Tailwind-like styling still lets us reach the clean T3 Code look.

## Consequences

- gpui's API changes often and is thinly documented. Pin its version and expect to read Zed's source.
