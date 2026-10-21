---
title: License
description: AGPL server, MIT plugin SDK, and what that means in practice.
group: Reference
source: README.md
---

tuff is split between two licenses:

- The server (everything not listed below) is licensed under the
  [GNU Affero General Public License v3.0 only](LICENSE) (`AGPL-3.0-only`).
- The plugin-facing code is licensed under the [MIT License](examples/LICENSE-MIT):
  the plugin SDK (`crates/tuff-plugin-sdk`, including its C and C++ headers),
  the SDK generator (`crates/tuff-plugin-sdk-gen`), the plugin ABI WIT files
  (`crates/tuff-plugin-wasm/wit`) and the example plugins (`examples/`). Each
  of these directories carries its own `LICENSE-MIT`, and the source files
  carry `SPDX-License-Identifier: MIT` headers.

What this means in practice:

- Server operators: running an unmodified tuff server places no obligations
  on you. If you run a modified server, the AGPL network-use clause (section
  13) requires you to offer the complete corresponding source of your
  modified version to everyone who interacts with it over the network,
  including players.
- Plugin authors: plugins written against the MIT SDK and ABI are your own
  work. You may license them however you like, including closed source, and
  the AGPL does not apply to them.

Contributions require agreeing to the [Contributor License Agreement](CLA.md);
see [CONTRIBUTING.md](CONTRIBUTING.md).
