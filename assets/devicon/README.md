# Devicon artwork

652 unmodified SVGs from [Devicon](https://devicon.dev/), pinned to
[`32fe1d3b3f36ad11f90ab94f7db59738139c2bf6`](https://github.com/devicons/devicon/tree/32fe1d3b3f36ad11f90ab94f7db59738139c2bf6).
Each brand uses its original, plain, or line SVG, in that order, without wordmarks.
The full-color SVGs are embedded in the executable and decoded lazily by GPUI; no runtime
network request or external font is needed. Their IDs use `devicon:` to avoid collisions
with Lucide icons and remain stable in saved libraries.

The upstream MIT license is included in [LICENSE](LICENSE). Product names, logos and brands
belong to their respective owners; their use identifies technologies and does not imply
endorsement. See the [upstream brand notice](https://devicon.dev/).

Run `python tools/update-devicon.py` to reproduce the assets and Rust catalog from the pinned
revision. Deliberately change the revision in that script when updating the bundle.
Command Prompt retains the existing Windows Terminal artwork because Devicon has no dedicated
Command Prompt logo. Existing `shell:` icon IDs remain supported for saved libraries.
