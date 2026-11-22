# Third-party notices

Zest bundles htmx 2.0.10 as `assets/htmx.min.js` under the Zero-Clause BSD
license (SPDX `0BSD`). The upstream license text is reproduced in
[`assets/HTMX-LICENSE.txt`](assets/HTMX-LICENSE.txt). [Upstream release](https://github.com/bigskysoftware/htmx/tree/v2.0.10) · [License](https://github.com/bigskysoftware/htmx/blob/v2.0.10/LICENSE)

Rust dependencies and exact versions are listed in `Cargo.lock` in the source
tree. Binary archives include dependency license files and the Rust standard
library notices under `licenses/`.

The crates.io packages for `maud` and `maud_macros` 0.27.0 omit the license
files from the upstream source tree. The packaging sources retain both files
from the commit recorded in those packages. [Maud 0.27.0 source](https://github.com/lambda-fairy/maud/tree/fb193d31462eea63eabfa705c93ccc56f5bb1eac)

The static Linux builds use the musl libc bundled with Rust's musl targets.
Binary archives include the copyright notice from musl 1.2.5. [musl 1.2.5 source archive](https://www.musl-libc.org/releases/musl-1.2.5.tar.gz)
