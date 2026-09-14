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

mod def_path;
mod format_args;
mod if_else_view;
mod imports;
mod manual_identifiable;
mod manual_text_map;
mod needless_anyview;
mod normalized_radius_overflow;
mod param_bounds;
mod qualified_waterui_path;
mod signal_get_in_view;
mod snapshot_get;
mod watch;
mod watch_ignores_value;
mod watch_over_collection;

const LINTS: &[&LintInfo] = &[
    &if_else_view::LINT_INFO,
    &manual_identifiable::LINT_INFO,
    &manual_text_map::LINT_INFO,
    &needless_anyview::LINT_INFO,
    &normalized_radius_overflow::LINT_INFO,
    &qualified_waterui_path::LINT_INFO,
    &signal_get_in_view::LINT_INFO,
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
    lint_store.register_late_pass(|_| Box::new(if_else_view::IfElseView::default()));
    lint_store.register_late_pass(|_| Box::new(manual_identifiable::ManualIdentifiable));
    lint_store.register_late_pass(|_| Box::new(needless_anyview::NeedlessAnyview));
    lint_store
        .register_late_pass(|_| Box::new(normalized_radius_overflow::NormalizedRadiusOverflow));
    lint_store
        .register_late_pass(|_| Box::new(qualified_waterui_path::QualifiedWateruiPath::default()));

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
    dylint_testing::ui_test_examples(env!("CARGO_PKG_NAME"));
}
