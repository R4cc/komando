# Highlights

Manage rule sets in **Settings → Highlights**. The included **Linux & RHEL** set has 62 rules
for Linux logs and common Red Hat Enterprise Linux administration tools.

The default palette uses vivid red for failures, bright amber for warnings, sage for successful states,
blue for information, lavender for component and unit names, and slate for quiet states and
debug output. Failures and warnings stay saturated so they stand out in the minimap as well
as the terminal. Only serious failures use bold text. Rules colour diagnostic words and status
values, leaving surrounding text and timestamps alone. Colours are checked at 4.5:1 contrast
or better against all fourteen bundled Terminal Themes, including light mode.

The minimap slightly enlarges colored rule matches at their original text positions. Red
failure matches get a little extra width; informational and warning matches get the same
thickness so they remain visible in error-heavy logs. A gap between rows prevents dense
errors from covering neighboring output. Emphasis follows enabled highlight rules, including
custom sets; ordinary text and ANSI colors alone do not receive this extra emphasis.

The set covers:

- **systemd and journal output:** service states, unit names, exit codes, failed jobs and
  kernel diagnostics. `active (exited)` is a successful state; `inactive (dead)` and `Stopped`
  are quiet states rather than failures. Zero-error counters do not turn red.
- **SELinux and Audit:** AVC denials, policy errors, enforcing and permissive modes.
  A denial with `permissive=1` on the same row is amber because the operation was allowed;
  ordinary denials are red. See Red Hat's [SELinux troubleshooting guide](https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/9/html/using_selinux/troubleshooting-problems-related-to-selinux_using-selinux).
- **DNF/YUM and RPM:** transaction stages, completion, repository metadata and GPG failures,
  missing dependencies, RHSA/RHBA/RHEA advisories and CVE identifiers.
- **Subscriptions:** registration problems and status values. `Overall Status: Disabled`
  stays informational: [Red Hat documents it as expected with Simple Content Access](https://access.redhat.com/solutions/7080864).
- **Networking and common components:** NetworkManager state changes and device states,
  SSH authentication, firewalld, SSSD, chronyd, Podman, Pacemaker and Corosync.

Rules use word boundaries to avoid matches inside unrelated words such as `ERRATA` or
`DEBUGGER`. Component names share one colour and do not imply a health state.

- **Order matters.** Rules apply top to bottom and the first match wins, so put specific rules
  (a permissive AVC denial or zero-error counter) above general severity words.
- **Format.** Same as Tabby's highlight plugin, so sets move between the two apps:

  ```json
  { "id": "…", "name": "Ops", "keywords": [
      { "text": "\\bERROR\\b", "enabled": true, "isRegExp": true, "foreground": true, "foregroundColor": "#FF4D4F", "isCaseSensitive": true },
      { "text": "health:\\s*(starting)", "isRegExp": true, "isMatchGroup": true, "matchGroup": "1", "foreground": true, "foregroundColor": "#87BFEA" }
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
- **Default updates.** Saved, untouched copies of the old `ADJUSTED` set and the earlier,
  muted `Linux & RHEL` set automatically use the new bundle. Renamed, reordered, recoloured
  or individually toggled sets stay as saved;
  deleted sets stay deleted. The set keeps its id, so a disabled default stays disabled.
  To replace a customized copy, import [the current bundled set](../assets/highlights/default.json).
