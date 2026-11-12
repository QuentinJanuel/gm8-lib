# gm8-lib

Reads, patches and writes GameMaker 8 game files.

Built on `gm8exe` and `gml-parser`, extracted from
[OpenGMK](https://github.com/OpenGMK/OpenGMK).

## Relation to OpenGMK

This repository carries OpenGMK's history up to commit `ca910f1`, and one commit on top of it that
turns that tree into this library: the emulator and the rest of OpenGMK are removed, the three
crates `gml-parser`, `gm8exe` and `gm8decompiler` are kept and extended, and `gm8patch`,
`gm8-detect` and `gm8-sprites` are new. `git diff ca910f1 HEAD` is the complete list of changes.
It is not a GitHub fork, because it is a different set of crates, but nothing here would exist
without the OpenGMK authors' work; see [NOTICE](NOTICE).

| Crate | Does |
| --- | --- |
| `gml-parser` | GML lexer and parser |
| `gm8exe` | Reads and writes GameMaker 8.0/8.1/8.2 executables; installs mod resources into parsed assets (`mods`); summarises a game's code (`game_code`) |
| `gm8patch` | `gm8exe`'s patcher behind one wasm export; no DOM, GL or audio |
| `gm8decompiler` | Executable to `.gmk` |
| `gm8-detect` | Whether a file is a GameMaker 8.0/8.1 executable `gm8exe` can read, as a plain C ABI wasm module |
| `gm8-sprites` | A GameMaker 8 executable's sprite names and RGBA frames, as a plain C ABI wasm module |

## Build and test

```sh
cargo build --workspace
cargo test -p gml-parser -p gm8exe
```

The patcher as a Node module (needs the `wasm-bindgen` CLI matching `Cargo.lock`):

```sh
cargo build --release --target wasm32-unknown-unknown -p gm8patch
wasm-bindgen --target nodejs --no-typescript --out-dir dist/patch \
  target/wasm32-unknown-unknown/release/gm8patch.wasm
```

`gm8-detect` and `gm8-sprites` need no glue; each `src/lib.rs` documents its exports:

```sh
cargo build --profile wasm-small --target wasm32-unknown-unknown -p gm8-detect -p gm8-sprites
```

## Checking against real games

The unit tests only prove that the reader and writer agree with each other. Two examples compare
against real files:

```sh
cargo run --release --example parse_real_game -p gm8exe -- game.exe...
cargo run --release --example bodydiff -p gm8exe -- game.exe   # what we write vs what the exe holds
```

Neither says whether GameMaker accepts the result. Only running a patched game does.

GPL-2.0-only. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
