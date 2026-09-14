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

## Lints

| Lint | Group | Description | Autofix |
| ---- | ----- | ----------- | ------- |
| `if_else_view` | `waterui_style` (warn) | Flags `if`/`else` that produce views — `when(cond, \|\| ..).otherwise(\|\| ..)` needs no `AnyView` erasure and stays reactive; conditions that `.get()` a signal are flagged as one-shot snapshots | Yes |
| `manual_identifiable` | `waterui_style` (warn) | Flags `impl Identifiable` whose `id` returns a field verbatim and `use_id`/`self_id` wrappers on local structs — `#[derive(Identifiable)]` plus `#[id]` on the field covers both | Yes |
| `manual_text_map` | `waterui_style` (warn) | Flags `.map(\|v\| format!(..))`/`.to_string()` over signals whose result only feeds a text position — `text!` formats the signal itself and keeps the template translatable | Yes |
| `needless_anyview` | `waterui_style` (warn) | Flags `AnyView::new`/`.anyview()` erasure where `impl View` is already accepted — `AnyView` is only needed for `AnyView` fields, `-> AnyView` signatures, `Vec<AnyView>`, and mixed-type branches | Yes |
| `normalized_radius_overflow` | `waterui_correctness` (deny) | Flags `RoundedRectangle`/`UnevenRoundedRectangle` radius constants above `0.5` — radii are fractions of the shorter side, not points | No |
| `qualified_waterui_path` | `waterui_style` (warn) | Flags `waterui::…`/`nami::…` paths (incl. `text!`/`debug!` macros) written qualified instead of imported — suggests `use` plus qualifier strip, or an `as` alias on a name collision | Yes |
| `set_with_own_get` | `waterui_style` (warn) | Flags `b.set(..b.get()..)` — the snapshot is stale by the time `set` runs; suggests `*b.get_mut() op= x`, `b.toggle()`, or `b.with_mut(\|v\| ..)` | Yes |
| `signal_get_in_view` | `waterui_correctness` (deny) | Flags `.get()` snapshots passed to reactive/view parameters (`IntoSignal`, `IntoComputed`, `IntoText`, `IntoLabel`, `View`, `ViewBuilder`) — the view freezes at the first read | Yes |
| `watch_for_reactive_value` | `waterui_suspicious` (warn) | Flags `watch`/`Dynamic::watch` whose single-expression closure reads the value only through carriers into signal-taking parameters (`IntoSignal`, `IntoComputed`, `IntoSignalF32`, `IntoText`, `IntoLabel`) — pass the signal itself so only that property updates and the subtree keeps its state | No |
| `watch_ignores_value` | `waterui_suspicious` (warn) | Flags `watch`/`Dynamic::watch` whose closure never reads the watched value — every change rebuilds an identical subtree; `.visible`/`when`/`.on_change` fit better | No |
| `watch_over_collection` | `waterui_suspicious` (warn) | Flags `watch`/`Dynamic::watch` over a `Collection` value (`Vec`, `reactive::collection::List`, `SignalCollection`, …) — every change rebuilds every row; `Lazy::for_each`/`List::for_each`/`VStack::for_each` or `SignalCollection` fit better | No |

Lint groups: `waterui_correctness` (deny), `waterui_suspicious` (warn), `waterui_style` (warn), `waterui_pedantic` (allow). Lint and group names are plain identifiers, so `#[allow(normalized_radius_overflow)]` and `-W waterui_pedantic` work as with any rustc lint; a `waterui::`-scoped spelling would need the linted crate to `#![register_tool(waterui)]`, which is unstable.

## Developing

Lint behavior is verified with UI tests. Each lint has a fixture package under `ui/<lint>/` (its own `Cargo.toml` and `main.rs`) and expected diagnostics in `ui/<lint>/main.stderr`.

Run all UI tests with:

```sh
cargo test
```

To build the lint library and run it against a fixture manually (the
`DYLINT_LIBRARY_PATH` points `--lib` at the freshly built library in
`target/debug`):

```sh
cargo build
DYLINT_LIBRARY_PATH="$PWD/target/debug" \
    cargo dylint --lib waterui_lints --manifest-path ui/<lint>/Cargo.toml
```

To create or update ("bless") expected diagnostics, run the `cargo dylint` command above, review its output carefully, and write the `stderr:` diagnostics into `ui/<lint>/main.stderr` (matching rustc's output format). Alternatively, run `cargo test`; on a mismatch the harness prints an `Actual stderr saved to ...` path that can be copied over `main.stderr` after review.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
