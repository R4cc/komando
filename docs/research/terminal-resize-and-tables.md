# Terminal resize and already-printed tables

Research date: 2026-11-12. Repository inspected at `632b0457605f2e78bae9044e9e3cb63e0683977d`, with a clean working tree before this document. Scope: source inspection and current primary-source research only. No application run, user-bug reproduction, test execution, SSH/homelab access, dependency update, or implementation was performed.

## Answer and practical recommendation

**Komando could preserve an existing table's geometry entirely in the emulator, provided the relevant cells remain available. It cannot generically recalculate arbitrary already-printed tables to fit a narrower terminal correctly.** Preservation is feasible as a new feature; the inspected code does not expose a fixed-width output viewer. Preserving geometry means retaining the original rows and columns and showing overflow through horizontal scrolling, clipping with retained off-screen content, or an explicit zoom view. Responsive table layout requires information that ordinary terminal output does not reliably carry: cell boundaries, original values, alignment rules, and column priorities. This is an inference from the cell-based terminal model and the table-producing application's layout responsibilities, described below.

Recommend retaining normal terminal resize behavior and first exploring an **explicit, read-only “View selection at fixed width” action**. Capture retained cells into an independent view before destructive loss; preserve original geometry and allow horizontal scrolling. That offers a narrowly defined emulator-only benefit without changing the live application's terminal coordinates. Treat automatic inline table preservation as a later architecture project, not a renderer tweak or a promise to repair arbitrary tables.

If the desired behavior is “the columns get narrower and the borders are redrawn around newly wrapped values,” a cooperating application, structured output format, or explicitly limited table parser is needed. Parsing known table formats is possible; universal reconstruction is not reliable. A snapshot also cannot recover values an application never printed.

## Observed facts: resize protocol and responsibilities

1. **SSH communicates dimensions, not a redraw request for old output.** RFC 4254 §6.7 defines `window-change` with columns, rows, pixel width, and pixel height; the reply flag is false and a response should not be sent. Ordinary channel data is a byte string (§5.2). These messages provide no table schema or instruction to rerun a completed command. [RFC 4254][P1]
2. **On Unix, changing PTY size notifies the foreground process group.** `TIOCSWINSZ` sets the window size; when it changes, the kernel sends `SIGWINCH` to that group. This is notification, not kernel-driven repainting of printed output. [Linux man-pages][P2]
3. **OpenSSH demonstrates the bridge between those mechanisms.** `session_window_change_req` reads the four dimensions and calls `pty_change_window_size`; that function invokes `ioctl(..., TIOCSWINSZ, ...)`. This verifies one implementation, not every SSH server. [OpenSSH session.c, lines 1732–1742][P3], [sshpty.c, lines 133–146][P4]
4. **Windows Local Sessions use a different mechanism.** `ResizePseudoConsole` changes ConPTY's internal buffers and the dimensions attached console applications observe. It is not a Unix `SIGWINCH` path. [Microsoft API documentation][P5]

**Inference:** a running editor or TUI can respond by rendering a new screen from its own state. A completed `ls`, SQL query, or one-shot table printer no longer owns a live redraw loop for its old output. Resizing may redraw the current shell prompt, but it does not make the exited producer regenerate earlier results. A still-running streaming program is also not guaranteed to redraw previous rows.

## Observed facts: Komando's current path

The references below describe the checked-out code, not runtime verification. Links target repository files; line numbers refer to the inspected revision.

| Area | Observed implementation | Code reference |
| --- | --- | --- |
| Dependency | Manifest requests `alacritty_terminal = "0.26"`; lockfile resolves `0.26.0`. | [Cargo.toml:13](../../Cargo.toml#L13), [Cargo.lock:182–203](../../Cargo.lock#L182) |
| Pane resize | Prepaint derives columns and rows from pixel bounds and cell metrics, calls `Term::resize`, drops blank-only Scrollback when applicable, updates report size, and forwards the size to the sink. Minimum is two columns and one row. | [src/ui/terminal_view.rs:1879–1913](../../src/ui/terminal_view.rs#L1879) |
| Initial size | `assume_size` resizes an unpainted terminal; otherwise an initial fallback is 120×36 until measured. | [src/ui/terminal_view.rs:300–317](../../src/ui/terminal_view.rs#L300) |
| Backend dispatch | Local receives `Msg::Resize`; SSH receives `Command::Resize`. | [src/session/mod.rs:113–121](../../src/session/mod.rs#L113) |
| SSH PTY | Opens at the latest reported pane size; requests `xterm-256color`. Later changes call `channel.window_change(cols, rows, pixel_width, pixel_height)`. | [src/session/ssh.rs:323–340](../../src/session/ssh.rs#L323), [380–385](../../src/session/ssh.rs#L380) |
| SSH parsing | `Output` holds a VTE `Processor`, advances it into the shared `Term`, and handles synchronized-update wakeups/timeouts. It has no table data model or output-block archive. | [src/session/ssh.rs:464–491](../../src/session/ssh.rs#L464) |
| Local PTY | Spawns Alacritty's PTY event loop against the same shared grid; on Windows this is ConPTY. | [src/session/local.rs:1–35](../../src/session/local.rs#L1) |
| Terminal config | Sets Scrollback length and OSC 52 copy-only permission, inheriting the remaining defaults. No Komando reflow policy is supplied here. | [src/session/mod.rs:313–320](../../src/session/mod.rs#L313) |
| Rendering | Copies the current renderable cells, including column, character, attributes, and wide-character flags. Row layout places glyphs by cell rather than trusting fallback-font advances. Box drawing is handled as line art. | [src/ui/terminal_view.rs:1567–1583](../../src/ui/terminal_view.rs#L1567), [1916–1950](../../src/ui/terminal_view.rs#L1916) |
| Scrolling and hit testing | Wheel handler uses the vertical delta; normal-screen scrolling changes `display_offset`. Pointer columns clamp to the current grid width. No horizontal viewport is implemented in these paths. | [src/ui/terminal_view.rs:680–707](../../src/ui/terminal_view.rs#L680), [998–1032](../../src/ui/terminal_view.rs#L998) |
| Existing export | Exports text from the retained grid, drops colors, trims trailing whitespace, and normalizes CRLF. This is not a full-fidelity cell snapshot. | [src/session/mod.rs:345–363](../../src/session/mod.rs#L345) |

The architecture deliberately embeds SSH and uses native GPUI plus Alacritty. A separate fixed-width viewer is compatible with those decisions; replacing the backend with a web terminal is unnecessary. [ADR 0002](../adr/0002-embedded-ssh-client.md), [ADR 0003](../adr/0003-native-gpu-ui.md)

### Command Marks are useful anchors, not command-output blocks

Command Marks are inferred when Enter is pressed and the cursor later moves beyond the line. They retain the displayed prompt/command and a private hyperlink tag on the first cell. Row lookup can follow a common Scrollback shift or rescan for tags. Logical line reading follows `WRAPLINE`, with a limit of eight rows. [src/marks.rs:15–46](../../src/marks.rs#L15), [70–119](../../src/marks.rs#L70), [125–203](../../src/marks.rs#L125), [207–257](../../src/marks.rs#L207)

**Inference:** these anchors can help navigation or suggest a capture range, but Enter and cursor motion do not reliably establish process start/end or output ownership. Interactive input, background output, multiline commands, overwritten tags, and full-screen applications complicate segmentation. The module comment says tags follow reflow; that is an implementation intention, not proof that every resize case works. The existing tests cover scrolling, clearing, redraw on Enter, and wrapped command text; they do not exercise a width resize. [src/marks.rs:328–435](../../src/marks.rs#L328)

## Observed facts: Alacritty terminal 0.26.0

Inspected the installed registry source and browsed upstream. Its `.cargo_vcs_info.json` identifies upstream revision `94e7c8874e526b1e67b349d9ba30ddf81669119e`; the links below pin that revision or the exact crate version.

- `Term::resize` reflows the **normal grid**, whether active or inactive, and does not reflow the **alternate grid**. It also clears selection on a column-count change, updates tab stops, resets the scrolling region, and resizes damage tracking. `Term::Config` has no reflow switch. [term/mod.rs:334–354, 655–704][A1]
- `Grid::resize(reflow, lines, columns)` accepts a boolean. Width growth joins continuation rows using `WRAPLINE`; width shrink splits retained content into additional rows and sets wrap flags. It handles wide-character spacers and cursor position. The rebuilt storage is bounded by visible rows plus the configured Scrollback limit, so sufficiently large expansion into rows can evict old content. [grid/resize.rs:12–35, 100–146, 244–387][A2]
- **Turning reflow off is not lossless preservation.** Width shrink calls `Row::shrink`, which splits off cells past the new width. With `reflow == false`, `shrink_columns` does not retain the returned overflow. Width growth cannot restore those discarded cells. [grid/row.rs:68–86][A3], [grid/resize.rs:271–286][A2]
- The grid stores characters, colors, flags, and optional extra cell attributes. It contains no native “table cell” or “column definition.” `WRAPLINE` is a cell flag. [term/cell.rs:14–35, 123–139][A4]
- Local `Msg::Resize` dispatches to PTY `on_resize`; the Windows backend calls the ConPTY resize API. [event_loop.rs:86–96][A5], [tty/windows/conpty.rs:303–315][A6]

**Inference:** changing only `layout_row` cannot preserve cells already transformed or discarded upstream. Calling `grid_mut().resize(false, ...)` instead of `Term::resize` would also bypass terminal-level bookkeeping and leave the inactive screen untreated. A proper global reflow control would need a supported terminal-level API or a maintained dependency patch, with explicit overflow-storage semantics; the existing grid boolean is insufficient.

## Inference: why text reflow is not table layout

A producer such as Rich retains rows, columns, sizing, justification, and wrapping rules while constructing a table, then prints its result. Its documentation explicitly describes calculating column sizes and wrapping content to terminal width. The terminal receives the rendered characters, not the Rich `Table` object. [Rich table documentation][P6]

From that distinction:

- A table rendered at 100 columns can have every emitted character preserved after narrowing to 60 columns while its borders and column alignment look broken. Character reflow splits physical rows; it does not shrink table columns or rebuild borders around cell contents.
- Normal soft wraps have continuation metadata and can often be rejoined. Explicit line endings remain separate logical rows. A table producer's own multiline cell layout or hard-wrapped prose supplies no universal hint that those rows should later be combined or rearranged.
- Widening can unwrap soft continuations, but does not automatically stretch borders, discover omitted values, or decide new column widths. When all relevant cells and wrap metadata survive, reconstructing the previously emitted logical rows may still be possible even after an earlier resize.
- Spaces can be padding or actual data; `|`, `+`, and box-drawing characters can be table borders, diagrams, or literals. Output may use tabs, cursor addressing, carriage returns, erase operations, and style changes. Distinct original datasets can produce the same displayed text, particularly after truncation. A generic parser therefore cannot guarantee the original structure.
- Preserving a 100-column layout in a 60-column pane requires accepting overflow, reducing cell size, or changing the layout. It cannot simultaneously retain the original cell geometry, unchanged font size, and fit every column inside the narrower pane.

These are constraints and deductions, not a reproduction of a Komando defect. Plain monospace rendering is necessary for column alignment but does not solve the loss of table semantics.

## Observed facts: mature terminals limit or expose reflow

- **Alacritty:** normal/alternate-screen distinction above is an intentional policy in the checked dependency. It is not a table detector. [A1]
- **xterm.js:** its published `reflowCursorLine` option defaults to false because shells usually redraw that line themselves. Its API also documents disabled reflow for certain Windows PTY configurations, including older ConPTY builds. Current `Buffer.ts` gates reflow on Scrollback capability and, when a Windows build number is present, a supported ConPTY backend/build. [xterm.js API, reflowCursorLine and windowsPty][P7], [Buffer.ts, `_isReflowEnabled`][P8]
- **Konsole:** its current scrolling preferences source exposes a checkbox for reflow on terminal resize. This establishes a user-selectable policy; this inspection does not establish the default or claim lossless overflow retention when disabled. [Konsole preferences UI:247–252][P9]

**Inference:** selective or disabled reflow is an established compatibility tradeoff. These sources do not establish that terminals universally disable reflow for tables, or that disabling it alone repairs them. The xterm.js Windows workarounds are a reason to test Komando's Windows path independently, not evidence of a Komando double-reflow bug.

## OSC 133: useful optional metadata, not a table protocol

The FinalTerm/iTerm2 conventions mark prompt start (`A`), command-input start (`B`), output start (`C`), and completion (`D`, optionally with exit status). They do not describe table columns or original values. [iTerm2's protocol documentation][P10]

Komando currently parses through VTE 0.15.0. Its OSC dispatcher has no `133` branch and sends unknown OSCs to the unhandled path; there is no Komando shell-integration handler in the inspected Session/mark code. Merely enabling a shell script would not create usable output-block events. [VTE ansi.rs:1329–1524][A7], [src/session/ssh.rs:468–474](../../src/session/ssh.rs#L468), [src/marks.rs:70–95](../../src/marks.rs#L70)

**Proposal:** optionally add streaming-safe OSC 133 handling to improve block boundaries where a shell already emits the markers. Installing or injecting shell hooks is application-side cooperation and falls outside a strict “no shell changes” requirement. Keep manual selection usable without integration. Even with markers, background output and nested shells can blur ownership; delimiters alone do not make a stream a table or guarantee an immutable snapshot.

## Proposed features and staged approach

All items in this section are proposals, not existing capabilities or fixes validated by this research.

### Stage 0: establish the exact symptom and invariant

Capture a minimal synthetic output fixture, its terminal dimensions, whether the producer remains running, normal versus alternate screen, and whether it used explicit newlines or cursor motion. Separate three outcomes: expected character wrapping, actual loss/corruption of retained cells, and application redraw/prompt-coordinate errors. Record Windows version, shell, Session type, and dependency versions. Do not change resize timing or reflow policy until a failing case identifies the layer responsible.

The invariant for a preservation feature should be: **every captured cell and its relative row/column position remains accessible after viewport resize**, within explicit retention limits. A responsive-table feature needs a different contract and a defined input grammar.

### Stage 1: smallest emulator-only feature

Implement a separate read-only fixed-width view for a user-selected region of retained output. Snapshot cells, row boundaries, wrap metadata, and the relevant render attributes under the terminal lock; perform view layout after releasing it. Offer horizontal scrolling and selection/copy in that view. Do not feed viewer offsets into the live terminal's mouse reporting or query replies.

Define two capture interpretations explicitly: preserving the current physical cell rectangle, or joining known soft wraps into logical rows. The latter can recover a printable table's emitted rows when continuation metadata survives, but cannot infer missing data or reliably undo arbitrary cursor-based drawing. Do not use `scrollback_text` as a full-fidelity snapshot: it trims and strips information.

Keep forwarding actual live terminal sizes normally. A separately captured view is immutable and should not reflect later erases or app redraws. Bound its memory and keep it in memory initially; persistence is a separate decision. This stage requires no Alacritty fork or shell installation, but cannot promise to capture output already evicted or truncated.

### Stage 2: reliable capture and output boundaries

If users need automatic command-output capture, introduce a bounded archive of completed output blocks with stable IDs and recorded geometry. Establish how rows become immutable, how blocks span the active screen and Scrollback, and how multiple widths within one command are represented. Observe parsing/scroll transitions before destructive loss; a UI-only capture after resize is too late to guarantee preservation.

Use OSC 133 when available and offer manual range capture as the reliable fallback. Command Marks may locate candidates but should not be treated as authoritative process boundaries. Preserve color, wide/combining-character metadata, hyperlinks, and row endings without reusing private Command Mark hyperlinks as the archive identity.

An inline mixed view of archived blocks and a live grid would need a display-row index mapping for selection, copy, command navigation, minimap, and search if applicable. Start with the separate viewer to avoid imposing that complexity on the live terminal.

### Stage 3: optional advanced policies

- **Fixed terminal width with a horizontal viewport:** keep the emulation grid, PTY width, and query responses at the chosen width while only the visible viewport narrows. This preserves geometry prospectively but changes what all programs perceive and requires cursor visibility, selection, and mouse-coordinate translation. Handle actual grid resizing and alternate-screen transitions deliberately. It is a distinct mode, not “send 60 columns but emulate 100.”
- **Selective inline no-reflow blocks:** requires retained off-screen cells or canonical block storage and a terminal-level policy/API. A global boolean alone cannot preserve mixed prose and table geometry. Avoid bypassing `Term::resize` or silently editing wrap flags in the live grid.
- **Heuristic detection:** suggest fixed-width viewing for repeated borders/aligned separators, with an explicit override. Do not silently rewrite arbitrary output or infer schemas from every group of spaces.
- **True adaptive tables:** accept explicitly structured output or support a documented subset of table formats in a separate viewer. Obtain cell values and layout rules, then render at the new width. This is an optional structured-data feature, not a general VT resize fix.

Automatically rerunning the user's command is not an acceptable generic resize strategy: it can repeat side effects and produce different data. Explicit rerunning by the user or an application-owned redraw loop is a separate workflow.

## Existing test coverage and proposed verification

**Observed coverage, not executed here:** the Unix compatibility harness resizes the grid, reports size, and backend in the same order as the UI. Tests cover Vim, htop, and tmux reacting to resize, plus Bash and a throwaway local SSH server reporting updated `stty size`. They are ignored by default and skip missing programs. They do not establish preservation of completed bordered tables, full-fidelity overflow storage, or Windows ConPTY behavior. [src/compat_tests.rs:1–13](../../src/compat_tests.rs#L1), [203–207](../../src/compat_tests.rs#L203), [259–267](../../src/compat_tests.rs#L259), [299–336](../../src/compat_tests.rs#L299), [372–384](../../src/compat_tests.rs#L372), [470–490](../../src/compat_tests.rs#L470); the module is Unix-only: [src/lib.rs:4–5](../../src/lib.rs#L4).

Proposed tests, staged with the implementation:

1. **Deterministic parser/grid fixtures:** at 100×30 feed an ASCII bordered table, Unicode bordered table, and space/tab-aligned rows; use explicit `\r\n`. Add soft-wrapped prose as a control. Resize 100→60→120→100, with content both visible and in Scrollback. Compare retained logical text and attributes independently from physical row layout. Expected wrapping must not be mislabeled corruption.
2. **Boundary cases:** widths immediately below, equal to, and above a border's width; exact last-column writes and pending autowrap; repeated shrink/grow; height-only changes; cursor at the bottom; bounded/full Scrollback. Distinguish documented eviction from unexpected loss. Test output originally produced at a narrow width separately from output narrowed afterward.
3. **Character and drawing cases:** CJK, combining accents, emoji, colored padding, OSC 8 links, wide characters on the boundary, explicit multiline cells, literal border characters in values, CR rewrites, cursor addressing, and erase sequences. Check both copied text and the cell matrix; use visual checks for actual border seams and glyph alignment.
4. **Capture/viewer contract:** capture before resize, then verify all cells/geometry remain accessible through horizontal scrolling; shrinking and widening the viewer must not change its snapshot. Confirm live terminal dimensions still follow the pane. Verify selection/copy with a horizontal offset, wide-character clipping, retention limits, and incomplete/evicted capture warnings.
5. **Command Marks and integration:** verify mark identity and destination across width reflow, pending Enter, overwritten first cells, full Scrollback, and normal/alternate-screen switches. If OSC 133 is added, split markers across byte chunks and test missing, repeated, nested, and malformed sequences, BEL/ST endings, missing integration, and background output.
6. **Backend compatibility:** keep current Unix resize tests; add a small local PTY fixture that reports `SIGWINCH` and observed dimensions while retaining deterministic output. Use the existing isolated local-sshd harness for SSH forwarding; no homelab is needed. Test sustained output during repeated resizes and synchronized-update boundaries without assuming a network acknowledgment is a completed redraw.
7. **Windows acceptance:** run the actual Windows 11 app with PowerShell/PSReadLine and cmd Local Sessions and with an SSH Session. Repeat normal-screen tables, prompt editing, alternate-screen programs, pane resizing, and font-size changes. Verify ConPTY behavior on the tested Windows build; Linux PTY tests cannot substitute for this.
8. **Performance:** bound archive memory and capture cost; measure frame latency and parser-lock hold time with long Scrollback, many Command Marks, and rapid resize/output. Preserve the existing backend-thread parsing architecture; do not scan the entire Scrollback on every drag event.

Suggested future commands include `cargo test --locked --lib marks` and the specifically relevant ignored compatibility tests, with installed prerequisites and skip output inspected. Do not report a skip as a pass or use the whole `compat` prefix casually: that prefix also includes unrelated SFTP stress work. No command in this plan was executed for this research.

## Unverified bugs and limitations

- No exact user command, terminal recording, before/after cell dump, or screenshot was supplied. No user-reported failure was reproduced. There is no evidence here assigning a particular corruption symptom to Komando, Alacritty, a shell, ConPTY, or a remote application.
- Grid resize before asynchronous backend notification is observed. That alone does not establish a race bug. Resize storms, interleaved synchronized updates, prompt redraw, and stale coordinate issues are investigation targets only.
- Command Mark survival across arbitrary reflow is not demonstrated by the inspected mark tests. Potential interactions with hyperlink tags are unverified; no lost-mark defect is claimed.
- Snapshot reconstruction is conditional on retained cells and wrap metadata. Already clipped, erased, evicted, or producer-truncated content cannot be recreated from the current grid alone. Capturing raw bytes in the future would require geometry/control-state recording and replay design, not merely saving printable strings.
- Alacritty/VTE conclusions are tied to the locked versions. Other projects' documentation and `master` sources were checked on the research date; their behavior may change. Public source inspection does not replace live interoperability testing.
- No product code, version, lockfile, changelog, or commit was changed for this task. The next action should be a deterministic fixture for the reported output, followed by a separately scoped fixed-width selection viewer if preservation is the intended user experience.

## Primary sources

The citations above link directly to specifications, project documentation, and source code. Alacritty and VTE implementation links are pinned to the installed package revisions; other projects' moving source links reflect the research date.

[P1]: https://www.rfc-editor.org/rfc/rfc4254.html#section-6.7
[P2]: https://www.man7.org/linux/man-pages/man2/TIOCSWINSZ.2const.html
[P3]: https://github.com/openssh/openssh-portable/blob/master/session.c#L1732-L1742
[P4]: https://github.com/openssh/openssh-portable/blob/master/sshpty.c#L133-L146
[P5]: https://learn.microsoft.com/en-us/windows/console/resizepseudoconsole
[P6]: https://rich.readthedocs.io/en/stable/tables.html
[P7]: https://xtermjs.org/docs/api/terminal/interfaces/iterminaloptions/#optional-reflowcursorline
[P8]: https://github.com/xtermjs/xterm.js/blob/master/src/common/buffer/Buffer.ts#L293-L298
[P9]: https://github.com/KDE/konsole/blob/master/src/widgets/EditProfileScrollingPage.ui#L247-L252
[P10]: https://iterm2.com/documentation-escape-codes.html#shell-integration-finalterm
[A1]: https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/term/mod.rs#L655-L704
[A2]: https://docs.rs/alacritty_terminal/0.26.0/src/alacritty_terminal/grid/resize.rs.html#12-388
[A3]: https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/grid/row.rs#L68-L86
[A4]: https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/term/cell.rs#L123-L139
[A5]: https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/event_loop.rs#L86-L96
[A6]: https://github.com/alacritty/alacritty/blob/94e7c8874e526b1e67b349d9ba30ddf81669119e/alacritty_terminal/src/tty/windows/conpty.rs#L303-L315
[A7]: https://github.com/alacritty/vte/blob/3b3da71c34cc1256c7e20981cf03f8eb95e08ffc/src/ansi.rs#L1329-L1524
