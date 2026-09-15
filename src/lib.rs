#![feature(rustc_private)]
#![warn(unused_extern_crates)]

dylint_linting::dylint_library!();

// A list of available compiler crates can be found here:
// https://doc.rust-lang.org/nightly/nightly-rustc/
extern crate rustc_ast;
extern crate rustc_data_structures;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_lexer;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

use rustc_lint::{Lint, LintId, LintStore};

/// A `waterui` lint group. Each lint names its group in its
/// `declare_waterui_lint!` invocation, and the group fixes the lint's default
/// level: `correctness` lints deny, `suspicious` and `style` lints warn, and
/// `pedantic` lints are allow-by-default opt-ins.
///
/// Group names are plain identifiers (`waterui_style`), not tool-scoped
/// (`waterui::style`): rustc rejects a scoped name whose tool the linted crate
/// has not registered with `#![register_tool]`, and dylint registers none.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Correctness,
    Suspicious,
    Style,
    Pedantic,
}

impl Group {
    const ALL: [Self; 4] = [
        Self::Correctness,
        Self::Suspicious,
        Self::Style,
        Self::Pedantic,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Correctness => "waterui_correctness",
            Self::Suspicious => "waterui_suspicious",
            Self::Style => "waterui_style",
            Self::Pedantic => "waterui_pedantic",
        }
    }
}

/// The metadata a lint module hands back to the library: the lint plus the
/// group it was declared in. `declare_waterui_lint!` emits a `LINT_INFO`
/// static in each lint module; `LINTS` below collects them.
pub(crate) struct LintInfo {
    pub lint: &'static Lint,
    pub group: Group,
}

/// Declares a `waterui` lint and its group membership in one declaration.
/// `$group` is `correctness`, `suspicious`, `style`, or `pedantic` and sets
/// the lint's default level (`Deny`, `Warn`, `Warn`, `Allow` respectively).
macro_rules! declare_waterui_lint {
    ($(#[$attr:meta])* $vis:vis $NAME:ident, correctness, $desc:literal) => {
        rustc_session::declare_lint! { $(#[$attr])* $vis $NAME, Deny, $desc }
        pub(crate) static LINT_INFO: $crate::LintInfo =
            $crate::LintInfo { lint: $NAME, group: $crate::Group::Correctness };
    };
    ($(#[$attr:meta])* $vis:vis $NAME:ident, suspicious, $desc:literal) => {
        rustc_session::declare_lint! { $(#[$attr])* $vis $NAME, Warn, $desc }
        pub(crate) static LINT_INFO: $crate::LintInfo =
            $crate::LintInfo { lint: $NAME, group: $crate::Group::Suspicious };
    };
    ($(#[$attr:meta])* $vis:vis $NAME:ident, style, $desc:literal) => {
        rustc_session::declare_lint! { $(#[$attr])* $vis $NAME, Warn, $desc }
        pub(crate) static LINT_INFO: $crate::LintInfo =
            $crate::LintInfo { lint: $NAME, group: $crate::Group::Style };
    };
    ($(#[$attr:meta])* $vis:vis $NAME:ident, pedantic, $desc:literal) => {
        rustc_session::declare_lint! { $(#[$attr])* $vis $NAME, Allow, $desc }
        pub(crate) static LINT_INFO: $crate::LintInfo =
            $crate::LintInfo { lint: $NAME, group: $crate::Group::Pedantic };
    };
}

mod anyview;
mod binding;
mod blocking_in_ui_context;
mod carriers;
mod collection_item_snapshot;
mod config;
mod def_path;
mod empty_label_literal;
mod fixed_children_in_vec;
mod format_args;
mod format_in_text;
mod handler_captures_binding;
mod hardcoded_theme_value;
mod if_else_view;
mod imports;
mod list_in_scroll;
mod localized_concat;
mod long_text_key;
mod manual_identifiable;
mod manual_signal_combinator;
mod manual_text_map;
mod missing_translation;
mod needless_anyview;
mod non_reactive_ui_state;
mod normalized_radius_overflow;
mod on_change_derives_binding;
mod on_tap_on_control;
mod opacity_as_visibility;
mod param_bounds;
mod positional_state_ambiguity;
mod qualified_waterui_path;
mod redundant_anyview;
mod row_builder;
mod set_with_own_get;
mod signal_get_in_view;
mod snapshot_get;
mod spacer_in_zstack;
mod state_created_in_rebuilt_scope;
mod tap_gesture;
mod tappable_without_role;
mod task_handle_dropped;
mod text_key;
mod thread_sleep;
mod thread_sleep_in_test;
mod verbatim_text_literal;
mod watch;
mod watch_for_reactive_value;
mod watch_ignores_value;
mod watch_over_collection;

const LINTS: &[&LintInfo] = &[
    &blocking_in_ui_context::LINT_INFO,
    &blocking_in_ui_context::thread_sleep::LINT_INFO,
    &collection_item_snapshot::LINT_INFO,
    &empty_label_literal::LINT_INFO,
    &fixed_children_in_vec::LINT_INFO,
    &fixed_children_in_vec::push_loop_seed::LINT_INFO,
    &format_in_text::LINT_INFO,
    &format_in_text::plural_bypass::LINT_INFO,
    &handler_captures_binding::LINT_INFO,
    &hardcoded_theme_value::LINT_INFO,
    &if_else_view::LINT_INFO,
    &list_in_scroll::LINT_INFO,
    &localized_concat::LINT_INFO,
    &long_text_key::LINT_INFO,
    &manual_identifiable::LINT_INFO,
    &manual_signal_combinator::LINT_INFO,
    &manual_text_map::LINT_INFO,
    &missing_translation::LINT_INFO,
    &missing_translation::orphan::LINT_INFO,
    &needless_anyview::LINT_INFO,
    &non_reactive_ui_state::LINT_INFO,
    &normalized_radius_overflow::LINT_INFO,
    &on_change_derives_binding::LINT_INFO,
    &on_tap_on_control::LINT_INFO,
    &opacity_as_visibility::LINT_INFO,
    &positional_state_ambiguity::LINT_INFO,
    &qualified_waterui_path::LINT_INFO,
    &redundant_anyview::LINT_INFO,
    &set_with_own_get::LINT_INFO,
    &signal_get_in_view::LINT_INFO,
    &spacer_in_zstack::LINT_INFO,
    &state_created_in_rebuilt_scope::LINT_INFO,
    &state_created_in_rebuilt_scope::row_builder::LINT_INFO,
    &tappable_without_role::LINT_INFO,
    &task_handle_dropped::LINT_INFO,
    &thread_sleep_in_test::LINT_INFO,
    &verbatim_text_literal::LINT_INFO,
    &watch_for_reactive_value::LINT_INFO,
    &watch_ignores_value::LINT_INFO,
    &watch_over_collection::LINT_INFO,
];

#[expect(
    clippy::no_mangle_with_rust_abi,
    reason = "the dylint driver loads `register_lints` from the shared library by symbol name"
)]
#[unsafe(no_mangle)]
pub fn register_lints(sess: &rustc_session::Session, lint_store: &mut LintStore) {
    dylint_linting::init_config(sess);

    lint_store.register_lints(&LINTS.iter().map(|info| info.lint).collect::<Vec<_>>());
    lint_store
        .register_late_pass(|_| Box::new(blocking_in_ui_context::BlockingInUiContext::default()));
    lint_store.register_late_pass(|_| Box::new(collection_item_snapshot::CollectionItemSnapshot));
    lint_store.register_late_pass(|_| Box::new(empty_label_literal::EmptyLabelLiteral));
    lint_store.register_late_pass(|_| Box::new(fixed_children_in_vec::FixedChildrenInVec));
    lint_store.register_late_pass(|_| Box::new(handler_captures_binding::HandlerCapturesBinding));
    lint_store.register_late_pass(|_| Box::new(hardcoded_theme_value::HardcodedThemeValue));
    lint_store.register_late_pass(|_| Box::new(if_else_view::IfElseView::default()));
    lint_store.register_late_pass(|_| Box::new(list_in_scroll::ListInScroll));
    lint_store.register_late_pass(|_| Box::new(localized_concat::LocalizedConcat));

    // `text!` lowers its key into `format!` bytecode on HIR; the early pass
    // snapshots the default-format literal so the late text-key lints can
    // read it.
    let text_keys = text_key::TextKeys::default();
    lint_store.register_early_pass({
        let text_keys = text_keys.clone();
        move || Box::new(text_key::TextKeyCollector::new(text_keys.clone()))
    });
    lint_store.register_late_pass({
        let text_keys = text_keys.clone();
        move |_| Box::new(long_text_key::LongTextKey::new(text_keys.clone()))
    });
    lint_store.register_late_pass(|_| Box::new(manual_identifiable::ManualIdentifiable));
    lint_store.register_late_pass(|_| Box::new(manual_signal_combinator::ManualSignalCombinator));
    lint_store.register_late_pass(|_| Box::new(needless_anyview::NeedlessAnyview));
    lint_store.register_late_pass(|_| Box::new(non_reactive_ui_state::NonReactiveUiState));
    lint_store
        .register_late_pass(|_| Box::new(normalized_radius_overflow::NormalizedRadiusOverflow));
    lint_store.register_late_pass(|_| Box::new(on_change_derives_binding::OnChangeDerivesBinding));
    lint_store.register_late_pass(|_| Box::new(on_tap_on_control::OnTapOnControl));
    lint_store.register_late_pass(|_| Box::new(opacity_as_visibility::OpacityAsVisibility));
    lint_store
        .register_late_pass(|_| Box::new(positional_state_ambiguity::PositionalStateAmbiguity));
    lint_store
        .register_late_pass(|_| Box::new(qualified_waterui_path::QualifiedWateruiPath::default()));
    lint_store.register_late_pass(|_| Box::new(redundant_anyview::RedundantAnyview));
    lint_store.register_late_pass(|_| Box::new(set_with_own_get::SetWithOwnGet));

    // `format!(..)` loses its template when lowered to HIR; the early
    // collector keeps the AST `FormatArgs` so the late pass can rebuild a
    // `text!(..)` suggestion from it.
    let format_args = clippy_utils::macros::FormatArgsStorage::default();
    lint_store.register_early_pass({
        let format_args = format_args.clone();
        move || Box::new(format_args::FormatArgsCollector::new(format_args.clone()))
    });
    lint_store.register_late_pass({
        let format_args = format_args.clone();
        move |_| Box::new(format_in_text::FormatInText::new(format_args.clone()))
    });
    lint_store.register_late_pass({
        let format_args = format_args.clone();
        move |_| {
            Box::new(signal_get_in_view::SignalGetInView::new(
                format_args.clone(),
            ))
        }
    });
    lint_store.register_late_pass({
        let format_args = format_args.clone();
        move |_| Box::new(manual_text_map::ManualTextMap::new(format_args.clone()))
    });
    lint_store.register_late_pass({
        let text_keys = text_keys.clone();
        move |_| {
            Box::new(missing_translation::MissingTranslation::new(
                text_keys.clone(),
            ))
        }
    });
    lint_store.register_late_pass(|_| Box::new(spacer_in_zstack::SpacerInZstack));
    lint_store.register_late_pass(|_| {
        Box::new(state_created_in_rebuilt_scope::StateCreatedInRebuiltScope)
    });
    lint_store.register_late_pass(|_| Box::new(tappable_without_role::TappableWithoutRole));
    lint_store.register_late_pass(|_| Box::new(task_handle_dropped::TaskHandleDropped));
    lint_store.register_late_pass(|_| Box::new(thread_sleep_in_test::ThreadSleepInTest::default()));
    lint_store.register_late_pass(|_| Box::new(verbatim_text_literal::VerbatimTextLiteral));
    lint_store.register_late_pass(|_| Box::new(watch_for_reactive_value::WatchForReactiveValue));
    lint_store.register_late_pass(|_| Box::new(watch_ignores_value::WatchIgnoresValue));
    lint_store.register_late_pass(|_| Box::new(watch_over_collection::WatchOverCollection));

    for group in Group::ALL {
        lint_store.register_group(
            true,
            group.name(),
            None,
            LINTS
                .iter()
                .filter(|info| info.group == group)
                .map(|info| LintId::of(info.lint))
                .collect(),
        );
    }
}

#[test]
fn ui() {
    // `cargo test` injects `[env]`-table variables (e.g. a globally configured
    // `RUSTC_WRAPPER`) into the test process. A rustc wrapper makes the spawned
    // `cargo build --verbose` print `Running \`sccache rustc ...\`` lines that
    // `dylint_testing`'s flag extraction does not recognize as `rustc`
    // invocations, so the wrapper cannot be in effect here.
    unsafe {
        std::env::remove_var("RUSTC_WRAPPER");
        std::env::remove_var("RUSTC_WORKSPACE_WRAPPER");
    }
    // `WATERUI_LINTS_UI_EXAMPLE=<fixture>` scopes the run to one fixture so a
    // lint can be iterated on while sibling fixtures are still unblessed.
    match std::env::var("WATERUI_LINTS_UI_EXAMPLE") {
        Ok(example) => run_ui_example(&example),
        Err(std::env::VarError::NotPresent) => {
            // `ui_test_examples` cannot give each fixture its own
            // `CARGO_MANIFEST_DIR`, so enumerate the `ui/<example>/` packages
            // and run them one at a time.
            let ui_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui");
            let mut examples: Vec<String> = std::fs::read_dir(&ui_dir)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().join("main.rs").is_file())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
            examples.sort_unstable();
            for example in &examples {
                run_ui_example(example);
            }
        }
        Err(std::env::VarError::NotUnicode(raw)) => {
            panic!("WATERUI_LINTS_UI_EXAMPLE is not valid Unicode: {raw:?}");
        }
    }
}

/// Restores `CARGO_MANIFEST_DIR` on drop.
#[cfg(test)]
struct ManifestDirGuard(Option<std::ffi::OsString>);

#[cfg(test)]
impl ManifestDirGuard {
    fn set(dir: &std::path::Path) -> Self {
        let previous = std::env::var_os("CARGO_MANIFEST_DIR");
        unsafe { std::env::set_var("CARGO_MANIFEST_DIR", dir) };
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for ManifestDirGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.0 {
                Some(value) => std::env::set_var("CARGO_MANIFEST_DIR", value),
                None => std::env::remove_var("CARGO_MANIFEST_DIR"),
            }
        }
    }
}

/// Runs `ui_test_example` for `example` with `CARGO_MANIFEST_DIR` pointed at
/// the fixture package `ui/<example>`: `text!` reads `CARGO_MANIFEST_DIR/i18n`
/// at expansion time and the catalog lints (`missing_translation`,
/// `orphan_translation`) use the same channel, while the compiletest driver
/// compiles a tempdir copy of `main.rs` and inherits this process's
/// environment — so without this, a fixture's `i18n/` is invisible to both.
/// Cargo never reads the variable back, so the one-time library/driver build
/// and the flag-capture `cargo build` are unaffected.
#[cfg(test)]
fn run_ui_example(example: &str) {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join(example);
    let _guard = ManifestDirGuard::set(&manifest_dir);
    dylint_testing::ui_test_example(env!("CARGO_PKG_NAME"), example);
}
