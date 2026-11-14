# Local Shell artwork

The PowerShell and Command Prompt PNGs are native 256×256 artwork, replacing the
previous 32×32 images. They are embedded by `src/ui/widgets.rs` and rendered at the
requested UI size, including the display's scale factor.

| File | Source |
| --- | --- |
| `cmd.png` | The embedded 256×256 PNG frame from Windows Terminal's [`res/console.ico`](https://github.com/microsoft/terminal/blob/f6de9e0618015def1c9dda5b82f9d3abf03eb799/res/console.ico) |
| `powershell.png` | PowerShell's [`assets/Powershell_256.png`](https://github.com/PowerShell/PowerShell/blob/cb9cf48b76cf2fdafb2f2a5e4c64dad4da526486/assets/Powershell_256.png), for Windows PowerShell |
| `pwsh.png` | The embedded 256×256 PNG frame from PowerShell's [`assets/Powershell_black.ico`](https://github.com/PowerShell/PowerShell/blob/cb9cf48b76cf2fdafb2f2a5e4c64dad4da526486/assets/Powershell_black.ico), for PowerShell 7 |

The ICO frames are copied directly, without resizing or re-encoding.

`linux.png` remains the 32×32 Tux artwork from Windows Terminal's profile icons.
Windows Terminal's MIT license is retained in `LICENSE-windows-terminal.txt`.
PowerShell's separate asset and trademark notice is retained in
`NOTICE-powershell.md`; those assets are not covered by PowerShell's source-code
license.
