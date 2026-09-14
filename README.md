# lints

WaterUI-specific lints, packaged as a [dylint](https://github.com/trailofbits/dylint) library.

WaterUI views are plain Rust, so `rustc` and `clippy` already catch the mechanical mistakes. What they cannot see is the class of bug that compiles clean and behaves wrong: a `.get()` that freezes a signal into a snapshot, a `watch` that rebuilds a subtree the author meant to update, a `format!` that walks around the translation catalog, an `.opacity(0.0)` that hides a button from sight but not from the screen reader. These lints run inside the compiler on the expanded, type-resolved HIR, which is the only place those facts are known.

The lint catalog and its status live in this repository's issues. Each lint has a UI test under `ui/` and is documented in the crate's `README` table once it lands.

## Using

```bash
cargo install cargo-dylint dylint-link
```

Then, in the project's `Cargo.toml`:

```toml
[workspace.metadata.dylint]
libraries = [{ git = "https://github.com/water-rs/lints", tag = "v0.1.0" }]
```

and `cargo dylint --all`. The `water` CLI will front this as `water lint` (water-rs/waterui#735).

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
