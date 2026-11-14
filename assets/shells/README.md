# Local Shell artwork

The PowerShell and Command Prompt icons include native small-icon artwork as well as
the original 256×256 images. `src/ui/widgets.rs` picks the smallest frame that covers
the requested size in physical pixels, including the display's scale factor. A 15px
sidebar icon uses the 16px frame at 100% scaling and the 32px frame at 200%. The layout
size stays 15px; the large illustrations are retained for larger uses.

The small artwork has details and strokes designed for those sizes, instead of shrinking
the shading and fine detail of a 256px illustration into the sidebar.

| File | Source |
| --- | --- |
| `cmd.png` | The embedded 256×256 PNG frame from Windows Terminal's [`res/console.ico`](https://github.com/microsoft/terminal/blob/f6de9e0618015def1c9dda5b82f9d3abf03eb799/res/console.ico) |
| `powershell.png` | PowerShell's [`assets/Powershell_256.png`](https://github.com/PowerShell/PowerShell/blob/cb9cf48b76cf2fdafb2f2a5e4c64dad4da526486/assets/Powershell_256.png), for Windows PowerShell |
| `pwsh.png` | The embedded 256×256 PNG frame from PowerShell's [`assets/Powershell_black.ico`](https://github.com/PowerShell/PowerShell/blob/cb9cf48b76cf2fdafb2f2a5e4c64dad4da526486/assets/Powershell_black.ico), for PowerShell 7 |
| `cmd-{16,32,48}.png` | Native frames decoded from the same Windows Terminal `res/console.ico` |
| `pwsh-{16,20,24,32,40,48,64}.png` | Native frames decoded from the same PowerShell `assets/Powershell_black.ico` |
| `powershell-{16,32}.png` | Windows Terminal's [Windows PowerShell profile icon](https://github.com/microsoft/terminal/tree/f6de9e0618015def1c9dda5b82f9d3abf03eb799/src/cascadia/CascadiaPackage/ProfileIcons), `{61c54bbd-c2c6-5271-96e7-009a87ff44bf}.scale-{100,200}.png` |

The large ICO PNG frames and the profile PNGs are copied directly. Small ICO frames
are decoded to PNG without resizing, recolouring or redrawing.

`linux.png` remains the 32×32 Tux artwork from Windows Terminal's profile icons.
Windows Terminal's MIT license is retained in `LICENSE-windows-terminal.txt`.
PowerShell's separate asset and trademark notice is retained in
`NOTICE-powershell.md`; those assets are not covered by PowerShell's source-code
license.
