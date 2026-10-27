# Building and testing

## Build

```bash
cargo build --workspace            # debug
cargo build --release -p powermac  # release binary
./scripts/bundle-macos.sh          # release PowerMac.app (with the battery helper in Contents/MacOS and PowerMac Audio.driver in Contents/Resources; neither is installed)
```

Cross-check the Windows backends from a Mac:

```bash
rustup target add x86_64-pc-windows-msvc
cargo check --workspace --target x86_64-pc-windows-msvc
```

## Test

```bash
cargo test --workspace
```

About 550 tests cover the platform-independent logic: zone math, key remapping, MWB crypto and packets, scroll classification, rename transforms, image-resize math, color formats and more.

### Settings screenshots

```bash
PM_UI_SHOTS=/tmp/shots cargo test -p settings-ui shots
```

renders every settings page (light and dark) to PNGs. Several feature crates (clipboard history, color picker, file tools) have their own `shots` tests too.

### Mouse Without Borders interop

`crates/mwb/tests/interop.rs` builds a .NET harness from PowerToys' own encryption and packet code (needs `dotnet`). It checks byte-for-byte test vectors and runs live TCP sessions in both directions.

### Live checks

Some behaviour can only be checked on a real Mac with permissions granted: input capture and injection, snapping real windows, a real MWB peer, scroll reversal, key remaps (use harmless keys such as F13–F20) and Awake assertions (`pmset -g assertions`). Record what was run live in the release notes.

## Code layout

See [Crates](../architecture/crates.md). Each crate's `lib.rs` doc comment explains its design; `cargo doc --workspace --no-deps --open` renders them.
