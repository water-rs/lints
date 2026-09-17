//! `SignalExt::map`/`nami::map` call anatomy — splitting a call into its
//! receiver and function operand, reading the operand's single parameter
//! (a closure literal or a path naming a local `fn`/method), relating the
//! parameter pattern's bindings to the signal sources a `zip` flattens, and
//! testing expressions against those bindings. Shared by the lints that
//! inspect what a `map` does with the signal's value (`manual_text_map`,
//! `manual_string_signal`).

use clippy_utils::visitors::for_each_expr_without_closures;
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{
    Expr, ExprKind, HirId, ImplItemKind, ItemKind, Node, Pat, PatKind, QPath, TraitFn,
    TraitItemKind,
};
use rustc_lint::LateContext;
use rustc_middle::ty::{TyCtxt, TypeckResults};
use rustc_span::Span;
use rustc_span::symbol::{Ident, Symbol};
use std::ops::ControlFlow;

use crate::carriers::{is_call_to, strip_wraps};
use crate::param_bounds::call_args;

/// `SignalExt::map` and `nami::reactive_core::map::map` — the two spellings
/// of "transform this signal through a closure".
pub(crate) const MAP_FNS: &[&[&str]] = &[
    &["nami", "reactive_core", "ext", "SignalExt", "map"],
    &["nami", "reactive_core", "map", "map"],
];

/// `SignalExt::zip` and `nami::zip` — both splice several signals into one;
/// the lints flatten them into the map's signal sources.
pub(crate) const ZIP_FNS: &[&[&str]] = &[
    &["nami", "reactive_core", "ext", "SignalExt", "zip"],
    &["nami", "reactive_core", "zip", "zip"],
];

/// Calls a map result may pass through on its way to a text position. They
/// only rewrap the signal, so the analysis peels them and the fix drops them.
pub(crate) const RESULT_ADAPTERS: &[&[&str]] = &[
    &["nami", "reactive_core", "ext", "SignalExt", "computed"],
    &["nami", "reactive_core", "ext", "SignalExt", "with"],
    &["nami", "reactive_core", "ext", "SignalExt", "cached"],
    &["nami", "reactive_core", "ext", "SignalExt", "distinct"],
    &["nami", "reactive_core", "ext", "SignalExt", "inspect"],
    &[
        "nami",
        "reactive_core",
        "signal",
        "IntoSignal",
        "into_signal",
    ],
    &[
        "nami",
        "reactive_core",
        "signal",
        "IntoComputed",
        "into_computed",
    ],
];

/// `sig.map(f)` / `nami::map(sig, f)` → `(receiver, closure)`.
pub(crate) fn map_call<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Option<(&'hir Expr<'hir>, &'hir Expr<'hir>)> {
    if !is_call_to(cx, typeck, expr, MAP_FNS) {
        return None;
    }
    match call_args(expr).as_slice() {
        [receiver, func] => Some((*receiver, *func)),
        _ => None,
    }
}

/// The parameter pattern, body, and body's `TypeckResults` of `|pat| body` —
/// `map` closures take exactly one parameter. The closure is a nested body, so
/// its expressions must be typed through `typeck_body`, not `cx`'s table.
pub(crate) fn closure_parts<'hir>(
    tcx: TyCtxt<'hir>,
    func: &'hir Expr<'hir>,
) -> Option<(&'hir Pat<'hir>, &'hir Expr<'hir>, &'hir TypeckResults<'hir>)> {
    let mut func = func;
    while let ExprKind::DropTemps(inner) = func.kind {
        func = inner;
    }
    let ExprKind::Closure(closure) = func.kind else {
        return None;
    };
    let body = tcx.hir_body(closure.body);
    let [param] = body.params else {
        return None;
    };
    Some((param.pat, body.value, tcx.typeck_body(closure.body)))
}

/// `func` — a closure literal or a path naming a local `fn`/method
/// (`count.map(count_label)`) — as the triple every map-body analysis needs:
/// the single parameter's pattern, the body expression, and the body's own
/// `TypeckResults`. A `map` operand takes exactly one argument. Paths to
/// non-local or body-less items (foreign `fn`s, trait declarations) return
/// `None` — their bodies cannot be inspected.
pub(crate) fn mapped_fn<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    func: &'hir Expr<'hir>,
) -> Option<(&'hir Pat<'hir>, &'hir Expr<'hir>, &'hir TypeckResults<'hir>)> {
    if let Some(parts) = closure_parts(cx.tcx, func) {
        return Some(parts);
    }
    let mut func = func;
    while let ExprKind::DropTemps(inner) = func.kind {
        func = inner;
    }
    let ExprKind::Path(qpath) = func.kind else {
        return None;
    };
    let Res::Def(DefKind::Fn | DefKind::AssocFn, did) = typeck.qpath_res(&qpath, func.hir_id)
    else {
        return None;
    };
    let body_id = match cx.tcx.hir_node_by_def_id(did.as_local()?) {
        Node::Item(item) => match item.kind {
            ItemKind::Fn { body, .. } => body,
            _ => return None,
        },
        Node::ImplItem(item) => match item.kind {
            ImplItemKind::Fn(_, body) => body,
            _ => return None,
        },
        Node::TraitItem(item) => match item.kind {
            TraitItemKind::Fn(_, TraitFn::Provided(body)) => body,
            _ => return None,
        },
        _ => return None,
    };
    let body = cx.tcx.hir_body(body_id);
    let [param] = body.params else {
        return None;
    };
    Some((param.pat, body.value, cx.tcx.typeck_body(body_id)))
}

/// The signals the map reads: `zip(a, b)` / `a.zip(&b)` flatten to their
/// arguments (recursively, for nested zips); anything else is the single
/// source.
pub(crate) fn sources<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> Vec<&'hir Expr<'hir>> {
    let expr = strip_wraps(cx, typeck, expr);
    if is_call_to(cx, typeck, expr, ZIP_FNS) {
        return call_args(expr)
            .into_iter()
            .flat_map(|arg| sources(cx, typeck, arg))
            .collect();
    }
    vec![expr]
}

/// The bindings a map-closure parameter pattern makes, related to the
/// signal sources it destructures.
pub(crate) struct Params {
    /// Whole-source bindings — `v` in `|v|`, `x`/`y` in `|(x, y)|` over
    /// `zip(a, b)` — binding `HirId` → source index.
    pub(crate) source_of: FxHashMap<HirId, usize>,
    /// Every binding the pattern makes, partial destructures included.
    pub(crate) all: FxHashSet<HirId>,
    /// Binding name per source position — the slot name a non-bare receiver
    /// aliases to.
    pub(crate) names: Vec<Option<Symbol>>,
    /// The pattern's own span — reused in the help alias.
    pub(crate) span: Span,
}

/// Pattern positions in order — `|(x, (y, _))|` flattens to three slots;
/// positions that are not a bare binding (`_`, literals, nested structs)
/// record `None`.
fn flatten_pat(pat: &Pat<'_>, slots: &mut Vec<Option<HirId>>, names: &mut Vec<Option<Symbol>>) {
    match pat.kind {
        PatKind::Binding(_, hir_id, ident, _) => {
            slots.push(Some(hir_id));
            names.push(usable_ident(ident));
        }
        PatKind::Tuple(subpats, _) | PatKind::TupleStruct(_, subpats, _) => {
            for sub in subpats {
                flatten_pat(sub, slots, names);
            }
        }
        PatKind::Ref(inner, _, _) | PatKind::Box(inner) | PatKind::Deref(inner) => {
            flatten_pat(inner, slots, names);
        }
        PatKind::Or([first, ..]) => flatten_pat(first, slots, names),
        _ => {
            slots.push(None);
            names.push(None);
        }
    }
}

impl Params {
    pub(crate) fn new(pat: &Pat<'_>, source_count: usize) -> Self {
        let mut slots = Vec::new();
        let mut names = Vec::new();
        flatten_pat(pat, &mut slots, &mut names);
        let mut source_of = FxHashMap::default();
        // A binding names a whole source only when the pattern's arity
        // matches the source count — `|(x, y)|` over `zip(a, b)`, `|v|` over
        // a lone signal. Otherwise every binding is a partial destructure.
        if slots.len() == source_count {
            for (index, slot) in slots.into_iter().enumerate() {
                if let Some(hir_id) = slot {
                    source_of.insert(hir_id, index);
                }
            }
        } else {
            names = vec![None; source_count];
        }
        let mut all = FxHashSet::default();
        pat.each_binding(|_, hir_id, _, _| {
            all.insert(hir_id);
        });
        Self {
            source_of,
            all,
            names,
            span: pat.span,
        }
    }
}

/// A `path` expression resolving to one of `params` (`Res::Local`).
pub(crate) fn param_hir_id(expr: &Expr<'_>, params: &FxHashSet<HirId>) -> Option<HirId> {
    if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
        && let Res::Local(hir_id) = path.res
        && params.contains(&hir_id)
    {
        return Some(hir_id);
    }
    None
}

/// Whether `expr` mentions any of `params`, not descending into closures.
pub(crate) fn touches_param(expr: &Expr<'_>, params: &FxHashSet<HirId>) -> bool {
    for_each_expr_without_closures(expr, |e| {
        if param_hir_id(e, params).is_some() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_some()
}

/// A candidate slot name must be a plain identifier — the `text!`/`s!`
/// macros capture slots by name and parse `name = expr` bindings. Tuple
/// indices (`t.0` → `0`) and reserved names (`u.r#type` → `type`) do not
/// qualify.
pub(crate) fn usable_ident(ident: Ident) -> Option<Symbol> {
    let name = ident.name.as_str();
    (name
        .chars()
        .next()
        .is_some_and(|c| c == '_' || rustc_lexer::is_id_start(c))
        && name
            .chars()
            .all(|c| c == '_' || rustc_lexer::is_id_continue(c))
        && !ident.is_reserved()
        && !ident.is_special())
    .then_some(ident.name)
}

/// `expr` is a bare identifier path (`count`, `selection`) — the shape a
/// format slot can capture by name. `self.count` has two segments and does
/// not qualify.
pub(crate) fn bare_path_ident(expr: &Expr<'_>) -> Option<Symbol> {
    if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
        && let [segment] = path.segments
    {
        return usable_ident(segment.ident);
    }
    None
}

/// A name for an alias binding derived from the expression itself —
/// `u.name` → `name`, `t.0`/`u.r#type` → `arg{index}` (a field name that
/// is not a plain identifier cannot be a slot name).
pub(crate) fn alias_name(expr: &Expr<'_>, index: usize) -> String {
    match expr.kind {
        ExprKind::Field(_, ident) => usable_ident(ident)
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("arg{index}")),
        ExprKind::Path(QPath::Resolved(None, path)) => path
            .segments
            .last()
            .and_then(|segment| usable_ident(segment.ident))
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("arg{index}")),
        _ => format!("arg{index}"),
    }
}
