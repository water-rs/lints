use clippy_utils::diagnostics::span_lint_and_help;
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hir::Expr;
use rustc_hir::def_id::CRATE_DEF_ID;
use rustc_lint::{LateContext, LateLintPass};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, Symbol};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::text_key::{self, KeySpace, TextKeys};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Collects the translation keys `text!(..)` invocations and
    /// `Text::localized`/`localized_or` literals embed, and warns when a key
    /// is absent from one or more `i18n/*.toml` catalogs.
    ///
    /// The catalogs live next to the crate's `Cargo.toml` — `i18n/` under
    /// `CARGO_MANIFEST_DIR`, the same directory the `text!` macro reads at
    /// expansion. A crate with no `i18n/` directory is not the case this lint
    /// covers and stays silent.
    ///
    /// ### Why is this bad?
    ///
    /// A key absent from a catalog silently renders the key itself in that
    /// locale — a `fr.toml` missing `Cancel` shows an English "Cancel" where
    /// a French user expects "Annuler".
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text!("Save")  // i18n/en.toml has `"Save"`; i18n/fr.toml does not
    /// ```
    pub MISSING_TRANSLATION,
    pedantic,
    "a `text!`/`Text::localized` key absent from an `i18n/*.toml` catalog"
}

/// `ORPHAN_TRANSLATION` shares this module — one pass collects the keys both
/// lints compare against the catalogs — so its declaration lives in a
/// submodule to keep the two `LINT_INFO`s apart.
pub(crate) mod orphan {
    declare_waterui_lint! {
        /// ### What it does
        ///
        /// Warns for a key in `i18n/*.toml` that no `text!(..)` or
        /// `Text::localized`/`localized_or` literal in the crate uses —
        /// the mirror image of `missing_translation`, reported on the crate
        /// root since an unused key has no call site.
        ///
        /// ### Why is this bad?
        ///
        /// Orphan keys are dead catalog entries — usually a renamed or
        /// removed call site — and they keep the file misleading about which
        /// strings the app actually shows.
        ///
        /// ### Example
        ///
        /// ```toml
        /// # i18n/en.toml — no source literal uses "Legacy title"
        /// "Legacy title" = "Legacy title"
        /// ```
        pub ORPHAN_TRANSLATION,
        pedantic,
        "an `i18n/*.toml` key no `text!` or `Text::localized` literal uses"
    }
}

impl_lint_pass!(MissingTranslation => [
    MISSING_TRANSLATION,
    orphan::ORPHAN_TRANSLATION,
]);

/// Both lints' help text: key-set consistency *between* locale files is
/// `water doctor`'s job (water-rs/waterui#735); these lints check the
/// catalogs against the code.
const HELP: &str =
    "`water doctor` checks the catalogs against each other; this lint checks them against the code";

/// One `i18n/<locale>.toml` catalog.
struct Catalog {
    /// The file name, e.g. `en.toml`.
    file: String,
    /// Top-level TOML keys verbatim — the space `Text::localized*` keys
    /// compare in.
    raw: FxHashSet<String>,
    /// `raw` with `{#` rewritten to `{` — the space `text!` keys compare in,
    /// since the macro emits the default format already rewritten.
    format: FxHashSet<String>,
}

impl Catalog {
    /// Whether this catalog carries `key`, compared in the key's own space.
    fn contains(&self, key: Symbol, space: KeySpace) -> bool {
        match space {
            KeySpace::Catalog => self.raw.contains(key.as_str()),
            KeySpace::Format => self.format.contains(key.as_str()),
        }
    }

    /// Whether any collected source literal uses `key` (a top-level catalog
    /// key, verbatim).
    fn used(&self, key: &str, used: &FxHashMap<(Symbol, KeySpace), Span>) -> bool {
        used.contains_key(&(Symbol::intern(key), KeySpace::Catalog))
            || used.contains_key(&(Symbol::intern(&key.replace("{#", "{")), KeySpace::Format))
    }
}

/// Reads `path` as a flat TOML table — every top-level key is a translation
/// key whether its value is a string or a plural table. Not a `.toml` file,
/// unreadable, or unparsable: `None` (the `text!` macro itself reports
/// broken catalogs as compile errors, so a parse failure here is a file the
/// crate could never have compiled with).
fn read_catalog(path: &Path) -> Option<Catalog> {
    if path.extension() != Some(OsStr::new("toml")) {
        return None;
    }
    let table = toml::from_str::<toml::Table>(&std::fs::read_to_string(path).ok()?).ok()?;
    let raw: FxHashSet<String> = table.keys().cloned().collect();
    let format = raw.iter().map(|key| key.replace("{#", "{")).collect();
    Some(Catalog {
        file: path.file_name()?.to_string_lossy().into_owned(),
        raw,
        format,
    })
}

/// The `i18n/` directory beside the crate's manifest — `CARGO_MANIFEST_DIR`
/// is set by cargo for every rustc invocation, and the ui harness points it
/// at each `ui/<fixture>` package so `text!` and this lint see the fixture's
/// catalogs.
fn i18n_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?).join("i18n");
    dir.is_dir().then_some(dir)
}

/// Every `i18n/*.toml` catalog, sorted by file name so diagnostics are
/// deterministic. An empty result — no directory or no readable TOML — keeps
/// both lints silent.
fn catalogs() -> Vec<Catalog> {
    let Some(dir) = i18n_dir() else {
        return Vec::new();
    };
    let mut catalogs: Vec<Catalog> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| read_catalog(&entry.path()))
        .collect();
    catalogs.sort_by(|a, b| a.file.cmp(&b.file));
    catalogs
}

/// Collects the keys in `check_expr`, compares them against the catalogs in
/// `check_crate_post`.
pub(crate) struct MissingTranslation {
    /// `text!` keys collected by the early pass, keyed by macro call span.
    text_keys: TextKeys,
    /// Each collected key (in its space) → the span of its first occurrence.
    used: FxHashMap<(Symbol, KeySpace), Span>,
}

impl MissingTranslation {
    pub(crate) fn new(text_keys: TextKeys) -> Self {
        Self {
            text_keys,
            used: FxHashMap::default(),
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for MissingTranslation {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let Some(key) = text_key::localized_key(cx, &self.text_keys, expr) else {
            return;
        };
        self.used.entry((key.text, key.space)).or_insert(key.span);
    }

    fn check_crate_post(&mut self, cx: &LateContext<'tcx>) {
        let catalogs = catalogs();
        if catalogs.is_empty() {
            return;
        }
        let mut missing: Vec<(Symbol, Span, Vec<String>)> = self
            .used
            .iter()
            .filter_map(|(&(key, space), &span)| {
                let files: Vec<String> = catalogs
                    .iter()
                    .filter(|catalog| !catalog.contains(key, space))
                    .map(|catalog| format!("`{}`", catalog.file))
                    .collect();
                (!files.is_empty()).then_some((key, span, files))
            })
            .collect();
        // Source order: deterministic diagnostics across runs.
        missing.sort_by(|(a_key, a_span, _), (b_key, b_span, _)| {
            a_span
                .lo()
                .cmp(&b_span.lo())
                .then_with(|| a_key.as_str().cmp(b_key.as_str()))
        });
        for (key, span, files) in missing {
            span_lint_and_help(
                cx,
                MISSING_TRANSLATION,
                span,
                format!("`{key}` has no translation in {}", files.join(", ")),
                None,
                HELP,
            );
        }
        let mut orphans: Vec<(&str, &str)> = Vec::new();
        for catalog in &catalogs {
            for key in &catalog.raw {
                if !catalog.used(key, &self.used) {
                    orphans.push((catalog.file.as_str(), key.as_str()));
                }
            }
        }
        orphans.sort_unstable();
        let root = cx.tcx.def_span(CRATE_DEF_ID);
        for (file, key) in orphans {
            span_lint_and_help(
                cx,
                orphan::ORPHAN_TRANSLATION,
                root,
                format!(
                    "`{key}` in `i18n/{file}` is not used by any `text!` or `Text::localized` in this crate"
                ),
                None,
                HELP,
            );
        }
    }
}
