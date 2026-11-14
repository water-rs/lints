use rustc_hir::def_id::DefId;
use rustc_lint::LateContext;

/// Whether `def_id`'s `LateContext::get_def_path` segments equal `path`
/// exactly: `["crate", "module", "Item"]` — the defining crate's path, never a
/// facade re-export.
pub(crate) fn def_path_eq(cx: &LateContext<'_>, def_id: DefId, path: &[&'static str]) -> bool {
    cx.get_def_path(def_id)
        .iter()
        .map(|segment| segment.as_str())
        .eq(path.iter().copied())
}
