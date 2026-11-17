# craft-sandbox-vbox

![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)
![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-orange?logo=rust)
![MCP stdio](https://img.shields.io/badge/MCP-stdio-purple)
![ArtCraft harness](https://img.shields.io/badge/ArtCraft-harness-green)

A lean, zero-panic Rust library for controlling VirtualBox VMs headlessly to safely detonate, trace, and reverse-engineer untrusted binaries.

## Highlights
- **Zero Panic Guarantee**: Built under `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- **Clean-Room RAII Sandbox**: `SandboxSession` automatically takes a snapshot on start and forcibly restores the clean snapshot on drop or failure.
- **Pure-Rust & No FFI**: Interacts strictly through bounded `VBoxManage` CLI subprocesses with non-shell argument lists and `CREATE_NO_WINDOW`.
- **Full Guest Control**: Execute guest programs, capture output, and monitor state.
