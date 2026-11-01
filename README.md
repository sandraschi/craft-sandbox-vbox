# craft-sandbox-vbox

A lean, zero-panic Rust library for controlling VirtualBox VMs headlessly to safely detonate, trace, and reverse-engineer untrusted binaries.

## Highlights
- **Zero Panic Guarantee**: Built under `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.
- **Clean-Room RAII Sandbox**: `SandboxSession` automatically takes a snapshot on start and forcibly restores the clean snapshot on drop or failure.
- **Pure-Rust & No FFI**: Interacts strictly through bounded `VBoxManage` CLI subprocesses with non-shell argument lists and `CREATE_NO_WINDOW`.
- **Full Guest Control**: Execute guest programs, capture output, and monitor state.
