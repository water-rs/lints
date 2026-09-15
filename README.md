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
| `blocking_in_ui_context` | `waterui_suspicious` (warn) | Flags calls that block the UI thread's executor — `std::fs::*`, `std::net::*`, `std::process::Command::{output,status}`/`Child::wait`, `futures`/`smol`/`async_io` `block_on`, `reqwest::blocking::*`, `Mutex::lock`, `RwLock::{read,write}` — inside a `Handler` closure, an `async` block/`async fn` body, `View::body`, or `GpuView::render`; the path list is extendable via `dylint.toml`'s `blocking_in_ui_context_paths`; move the work to `waterui::task::spawn` and await it | No |
| `collection_item_snapshot` | `waterui_suspicious` (warn) | Flags a non-signal field of a `for_each` item read into an `IntoSignal`/`IntoComputed`/`IntoSignalF32` parameter — `for_each` diffs by id, so the snapshot never re-renders; derive a `Computed` or keep a `Binding` on the item | No |
| `empty_label_literal` | `waterui_correctness` (deny) | Flags an empty/whitespace-only string literal in an `IntoLabel` parameter (`button("")`, `toggle`, `slider`, `field`, …) or `.a11y_label("")` — the mandatory label is how screen readers and `waterui-testing` find the control; name it, or hide it with `.hide_label()`/`LabelDisplayMode::IconOnly` | No |
| `fixed_children_in_vec` | `waterui_pedantic` (allow) | Flags a literal `vec![..]` passed as stack contents (`vstack(vec![..])`, `VStack::new(.., vec![..])`, …) — a tuple is the framework's shape for a fixed set of children; suggests `(a, b, c)` | Yes |
| `format_in_text` | `waterui_suspicious` (warn) | Flags a `format!(..)` with letters in its template reaching an `IntoText`/`IntoLabel` parameter (through `.to_string()`/`String::from`/`Str::from`/`.into()`/`Text::verbatim`) — the result is untranslatable `Text::verbatim`; suggests the `text!` rewrite when every slot is a simple path | Yes |
| `handler_captures_binding` | `waterui_style` (warn) | Flags a closure in a `Handler`/`HandlerOnce` position (`action`, `on_tap`, `gesture`, …) that captures a `Binding` or `ReactiveList` — inject it with `.state(&b)` and take `State<Binding<T>>` so the handler can be a named function | No |
| `hardcoded_theme_value` | `waterui_pedantic` (allow) | Flags all-literal `Color::srgb*`/`p3`/`oklch`/`Srgb::*` constructors and `Text`/`Font`/`StyledStr::size` calls inside `-> impl View` functions and `View::body` — literals do not adapt to the color scheme or type scale; use theme tokens (`AccentColor`, …) and semantic fonts (`.font(Title)`, …) or a named constant | No |
| `if_else_view` | `waterui_style` (warn) | Flags `if`/`else` that produce views — `when(cond, \|\| ..).otherwise(\|\| ..)` needs no `AnyView` erasure and stays reactive; conditions that `.get()` a signal are flagged as one-shot snapshots | Yes |
| `localized_concat` | `waterui_pedantic` (allow) | Flags `+` on `Text` whose left operand is localized — a `text!` expansion, a `Text::localized*` call, or a `text(..)`/`Text::new(..)`/`.into_text()`/`.into()` of a `&'static str` — since concatenated fragments cannot be translated as a unit; make it one `text!` key with slots | No |
| `long_text_key` | `waterui_pedantic` (allow) | Flags a `text!("..")` literal or `Text::localized`/`localized_or` key over `long_text_key_words` words (`dylint.toml`, default 12) — the whole literal is the lookup key, so a copy edit orphans every translation; use a short `"$about_blurb"`-style key and put the prose in `i18n/<locale>.toml` | No |
| `manual_identifiable` | `waterui_style` (warn) | Flags `impl Identifiable` whose `id` returns a field verbatim and `use_id`/`self_id` wrappers on local structs — `#[derive(Identifiable)]` plus `#[id]` on the field covers both | Yes |
| `manual_signal_combinator` | `waterui_style` (warn) | Flags `signal.map(\|v\| ..)` whose closure restates a named `SignalExt` combinator — `!v`→`.not()`, `v == e`→`.equal_to(e)`, `v >/<=/…`→`.gt`/`.le`/…, `-v`→`.negate()`, `v.is_some()`/`is_none()`/`is_ok()`/`is_err()`, `v.is_empty()`/`len()`/`contains(e)`→`.str_*`, `if v {a} else {b}`→`.select(a, b)` — the combinator keeps the canonical `fn`-item signal identity | Yes |
| `manual_text_map` | `waterui_style` (warn) | Flags `.map(\|v\| format!(..))`/`.to_string()` over signals whose result only feeds a text position — `text!` formats the signal itself and keeps the template translatable | Yes |
| `needless_anyview` | `waterui_style` (warn) | Flags `AnyView::new`/`.anyview()` erasure where `impl View` is already accepted — `AnyView` is only needed for `AnyView` fields, `-> AnyView` signatures, `Vec<AnyView>`, and mixed-type branches | Yes |
| `non_reactive_ui_state` | `waterui_suspicious` (warn) | Flags a handler closure (`action`, `on_tap`, `gesture`, …) mutating a captured `RefCell`/`Cell`/`Mutex`/`RwLock`/atomic or touching `static mut`/`thread_local!`, and such fields on `View` types — the mutation never notifies the reactive graph; keep UI state in a `Binding<T>` | No |
| `normalized_radius_overflow` | `waterui_correctness` (deny) | Flags `RoundedRectangle`/`UnevenRoundedRectangle` radius constants above `0.5` — radii are fractions of the shorter side, not points | No |
| `on_change_derives_binding` | `waterui_pedantic` (allow) | Flags `.on_change(&a, \|v\| b.set(f(v)))` — the copy lags the source by one update and is a second source of truth; derive it once with `a.map(f)` and read the `Computed` | No |
| `on_tap_on_control` | `waterui_suspicious` (warn) | Flags `.on_tap`/`.on_tap_gesture*`/`.on_tap_haptic*`/`.gesture(TapGesture, ..)` on a control with its own activation (`Button`, `Toggle`, `Slider`, `Stepper`, `TextField`, `ListItem`, `Picker`, `NavigationLink`, `Menu`) — the gesture and the control's action compete for the same tap; use `.action(..)` | No |
| `opacity_as_visibility` | `waterui_suspicious` (warn) | Flags `.opacity(0.0)`, `sig.select(1.0, 0.0)`, and `sig.map(\|b\| if b { 1.0 } else { 0.0 })` used to hide a view — `.visible(..)` also marks it accessibility-hidden and non-hittable | Yes |
| `plural_bypass` | `waterui_suspicious` (warn) | Flags the `format_in_text` case that also splices an integer into prose — the number skips CLDR plural categories; `{#count}` in a `text!` key is the only channel that yields correct plural forms per locale | No |
| `positional_state_ambiguity` | `waterui_suspicious` (warn) | Flags a handler (`action`, `on_tap`, `gesture`, …) with two `State<T>` parameters of the same `T` — they bind to `.state(..)` calls by position, not name, so the handler can silently take the wrong binding; put both values in one `Clone` struct injected once | No |
| `push_loop_seed` | `waterui_pedantic` (allow) | Flags `ReactiveList::new()` followed in the same block by a `for` loop whose body is a single `list.push(..)` over a local sequence — `ReactiveList::from(vec)` seeds in one move | No |
| `qualified_waterui_path` | `waterui_style` (warn) | Flags `waterui::…`/`nami::…` paths (incl. `text!`/`debug!` macros) written qualified instead of imported — suggests `use` plus qualifier strip, or an `as` alias on a name collision | Yes |
| `redundant_anyview` | `waterui_style` (warn) | Flags `AnyView::new`/`.anyview()`/`ViewExt::anyview` on a value that is already `AnyView` — the second wrapper is dead code | Yes |
| `set_with_own_get` | `waterui_style` (warn) | Flags `b.set(..b.get()..)` — the snapshot is stale by the time `set` runs; suggests `*b.get_mut() op= x`, `b.toggle()`, or `b.with_mut(\|v\| ..)` | Yes |
| `signal_get_in_view` | `waterui_correctness` (deny) | Flags `.get()` snapshots passed to reactive/view parameters (`IntoSignal`, `IntoComputed`, `IntoText`, `IntoLabel`, `View`, `ViewBuilder`) — the view freezes at the first read | Yes |
| `spacer_in_zstack` | `waterui_suspicious` (warn) | Flags a `Spacer` element in the contents tuple of `zstack((..))`/`ZStack::new(.., (..))` — a spacer expands along the stack axis and a Z stack has none, so the child is inert; position with `.alignment(..)` on the `zstack` or use `absolute((..))` for an edge-anchored overlay | No |
| `state_created_in_rebuilt_scope` | `waterui_suspicious` (warn) | Flags `binding(..)`/`Binding::…`/`ReactiveList::…` constructors inside `watch`/`when`/`.or`/`.otherwise` closures — the scope rebuilds on every change and the state resets; own it one level up and move it in | No |
| `state_created_in_row_builder` | `waterui_pedantic` (allow) | Flags the same state constructors inside `ForEach::new`/`Lazy::for_each`/`List::for_each`/stack `for_each` row builders — row-local state is sometimes intended, so the lint is opt-in | No |
| `tappable_without_role` | `waterui_suspicious` (warn) | Flags `.on_tap`/`.on_tap_gesture*`/`.on_tap_haptic*`/`.gesture(TapGesture, ..)` on a non-control view whose chain has no `.a11y_role` or `.a11y_label` — the tap target is invisible to assistive tech and `waterui-testing` queries; add a role + label or use `button(..)` | No |
| `task_handle_dropped` | `waterui_correctness` (deny) | Flags a `task::spawn`/`spawn_local` handle dropped at once — a bare `spawn(..);` statement, `let _ = spawn(..);`, or a `let t = spawn(..);` never read in its block — the handle cancels the task on drop; suggests `.detach()` for the unbound shapes | Yes |
| `thread_sleep_in_ui` | `waterui_correctness` (deny) | Flags `std::thread::sleep` inside a `Handler` closure, `async` scope, `View::body`, or `GpuView::render` — it parks the executor thread outright; await `waterui::task::sleep(..)` in an `.action_async`/`.task(..)` future | No |
| `verbatim_text_literal` | `waterui_suspicious` (warn) | Flags a string literal reaching an `IntoText`/`IntoLabel` parameter through `to_string()`/`to_owned()`/`String::from`/`Str::from(_static)`/`.into()`/`Text::verbatim` — `&'static str` resolves through the localization catalog, `String`/`Str` never translate; pass the literal itself | Yes |
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

or a single fixture with:

```sh
WATERUI_LINTS_UI_EXAMPLE=<lint> cargo test
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
