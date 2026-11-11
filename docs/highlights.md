# Highlights

Manage rule sets in **Settings → Highlights**. A set of 69 rules for typical Linux and service
logs is included.

- **Order matters.** Rules apply top to bottom and the first match wins, so put specific rules
  (`WARNING`, `Active: inactive`) above general ones (`WARN`, `Active`).
- **Format.** Same as Tabby's highlight plugin, so sets move between the two apps:

  ```json
  { "id": "…", "name": "Ops", "keywords": [
      { "text": "ERROR", "enabled": true, "foreground": true, "foregroundColor": "#FF0000", "isCaseSensitive": true },
      { "text": "health:\\s*(starting)", "isRegExp": true, "isMatchGroup": true, "matchGroup": "1", "foreground": true, "foregroundColor": "#FFA500" }
  ] }
  ```

  Plain `text` matches anywhere in a line. `isRegExp` rules use Rust regex syntax, which covers
  the usual JavaScript syntax except look-around and back-references; rules that can't be used
  are listed in the panel. With `isMatchGroup`, only the capture group named in `matchGroup` is
  coloured. `background`/`backgroundColor` and `bold` work too.
- **Import / export.** Import accepts one set or a list of sets. A set with the same `id`
  replaces the existing one. Export writes one set. Fields Komando doesn't use are kept, so an
  exported file matches what was imported.
- **Where they apply.** Highlights are skipped in full-screen programs (vim, less, htop), and
  each screen row is matched on its own. On a light terminal, very bright colours are darkened
  so they stay readable.
- **Storage.** Sets are kept in `%APPDATA%\Komando\highlights.json`. Adding and editing
  individual rules inside the app is not available yet: edit the JSON and re-import.
