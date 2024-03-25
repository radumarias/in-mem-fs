A very basic implementation of an in-mem filesystem in Rust. \
It uses [fuser](https://crates.io/crates/fuser) crate to expose the system with `FUSE`.

# Features
Log level is controlled via env variable `RUST_LOG`. \
If uses [log](https://crates.io/crates/log) crate, possible levels are `trace`, `debug`, `info`, `warn`, `error` as defined [here](https://docs.rs/log/latest/log/#macros).

# Not yet implemented
- move/rename (mv)
- links
- xattr

## Usage
```
export RUST_LOG='info'
in_mem_fs --mount-point PATH
```
