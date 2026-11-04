# Local changes to rqs_lib

Vendored from https://github.com/Martichou/rquickshare (`core_lib`) at commit
`378d8ae969941bee4bf60ad34ac9cf8bb7005eb7` (version 0.11.5), GPL-3.0.

1. **Tolerate extra setup frames** (`src/hdl/inbound.rs`, `process_transfer_setup`):
   Newer Android builds can send extra frames during connection setup.
   Upstream failed the whole transfer with "Missing required fields"; these frames
   are now logged and skipped.
2. **Large-transfer download dir** (`src/lib.rs` `set_large_download`, used in
   `process_introduction`): transfers above a threshold go to a second directory,
   so the normal download dir can live in RAM.
3. **Build trimmed** (`build.rs`, `Cargo.toml`): compiles the protobufs only; the
   TypeScript bindings step and the demo binary are removed.
