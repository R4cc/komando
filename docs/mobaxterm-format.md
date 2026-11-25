# MobaXterm session-format evidence

On 2026-11-15, [REA](https://github.com/morluto/rea) 6.9.0 with Ghidra 12.1.4
statically inspected the supplied `MobaXterm_Personal_26.6.exe`, a native x86 PE.
Its SHA-256 was `e9a66144b3a1c4d32f8062b1eb34e26e9ed5298106abf1982b4654ab107f13a5`.
The analysis discovered 18,715 procedures and 23,245 defined strings. Those counts
describe Ghidra's inventory, not complete recovery of the program.

## Session escaping

REA's `search_strings`, `xrefs`, `procedure_pseudo_code` and `read_bytes` operations
identified the following helpers. Addresses are virtual addresses in this exact binary.

| Procedure | Observation | REA Evidence ID |
| --- | --- | --- |
| `0x00a43520` | Encodes `#`, `=`, `;`, `"`, `\|` as `__DIEZE__`, `__EQUAL__`, `__PTVIRG__`, `__DBLQUO__`, `__PIPE__` | `ev_6be1bb1237214a9e9d2050c415da81e9fe2db95e316133a5a2efa3810fe1fec8` |
| `0x00a436c0` | Decodes those five tokens using the same replacement helper with flags `3` | `ev_cd779686982523627f3bbd3cbfca39ab76941d4c472491e3cb5fe9fa8928a520` |
| `0x00413680` | Replacement bit `2` folds the search and input text; bit `1` repeats replacement for all matches | `ev_acfeac1e39c61dbfac51ca0535cace6d78bfe19f3db3b1b58c1378db6df31996` |
| `0x0040b838` | The folding helper calls `CharUpperBuffA` | `ev_f55c201a793fa377a3be7f4ee7ea2028e5645d17173f054b76e2d75063500a13` |
| `0x008c92c4` | Encodes a percent sign as `__PERCENT__` | `ev_79226723cd901ac40213f34968162ed6c927c5097dae369954b79de0712d4b2b` |
| `0x008d9830` | The session serializer calls `0x00a43520` at `0x008e1012`, `0x008e105c` and `0x008e1078` | `ev_b9c2b99bbdad762862c84f5fa0c9ecd44a1fc12e9ab161d55795709feef211c6` |

REA also read the literal bytes at `0x00a43628` to verify the punctuation paired
with each token (`ev_dedd6bf5f2e2bcda60aa333d4a59f04960e660de105d9e10ff925964555e59e8`).

This explains why an exported command such as `export MODE__EQUAL__prod` needs to
become `export MODE=prod`. The previous importer left the equals token in the command.
Komando now recognizes all six session tokens without regard to ASCII case. Gateway
lists are split on pipes after decoding, preserving hop order and empty user slots.

The nearby helpers at `0x00a43860` and `0x00a43a44` instead use `__EQQUAL__`,
`__PIIPE__` and `__DBLDOT__`. A traced consumer at `0x009d4220` handles recording
and naming macros, including the text "Please enter a name for this macro"
(`ev_f68222dca4fc933299e1f48a753ecd1eaa4a8657f1b51831d0fb586ec19d0974`).
These spellings are not treated as aliases by the session decoder. The regression
test keeps them literal inside a shell command.

## Import behavior

Independently of the binary analysis, checking Komando's import application found
that an absent SSH startup command was represented as unknown (`None`). That allowed
an existing Project's Startup Commands to run after import. MobaXterm's empty command
now becomes a known empty list, matching the handling of a direct SSH route. A test
imports such a session into a Project with a command and verifies it stays disabled.

The new parser test is synthetic data derived from the recovered codec, not an
export captured from MobaXterm's UI. Existing real export fixtures still cover plain
SSH, an SSH gateway, a proxy and RDP. No passwords or private keys were imported.

## Reproduction and limits

The initial overview can be reproduced with:

```powershell
$env:GHIDRA_INSTALL_DIR = 'C:\tools\ghidra_12.1.4_PUBLIC'
npx.cmd --yes rea-agents@6.9.0 analyze '<path-to-MobaXterm_Personal_26.6.exe>' --provider ghidra --format json
```

For focused queries in one persistent REA MCP session, use `open_binary` with
`provider_id: "ghidra"`, then `procedure_pseudo_code` with the addresses above.
The CLI equivalent is `rea function <exe> <address> --provider ghidra --format json`.
Windows Ghidra analysis is ephemeral, so separate CLI queries repeat initial analysis.

This was static analysis of the executable. It did not launch MobaXterm, connect to
servers, inspect personal configurations, or verify the complete 26.6 session schema.
One large session-loading procedure (`0x008cdd70`) exceeded the decompiler's 50 MiB
payload allowance; its full behavior remains unverified. Desktop field positions
remain based on the sources described in ADR 0018, not a claim of complete native
validation. The executable's hash was checked again after analysis.
