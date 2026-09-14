use clippy_utils::diagnostics::{span_lint_and_help, span_lint_and_sugg, span_lint_and_then};
use clippy_utils::fn_def_id_with_node_args;
use clippy_utils::macros::{FormatArgsStorage, find_format_arg_expr, root_macro_call_first_node};
use clippy_utils::res::MaybeQPath;
use clippy_utils::source::{SpanRangeExt, snippet_opt};
use clippy_utils::ty::{all_predicates_of, implements_trait};
use rustc_ast::format::{FormatArgsPiece, FormatArgumentKind};
use rustc_ast::{Crate as AstCrate, Expr as AstExpr, ExprKind as AstExprKind, FormatArgs};
use rustc_data_structures::fx::FxHashMap;
use rustc_errors::Applicability;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor, nested_filter};
use rustc_hir::{Expr, ExprKind, QPath};
use rustc_lexer::{FrontmatterAllowed, TokenKind, tokenize};
use rustc_lint::{EarlyContext, EarlyLintPass, LateContext, LateLintPass};
use rustc_middle::ty::{
    AssocContainer, ClauseKind, EarlyBinder, GenericArg, GenericArgsRef, PredicatePolarity, Ty,
    TyKind, TypeVisitableExt,
};
use rustc_session::impl_lint_pass;
use rustc_span::{Span, hygiene, sym};
use std::mem;

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `Signal::get`/`Binding::get` snapshots passed into a call whose
    /// parameter is bound by a reactive or view trait — `IntoSignal`,
    /// `IntoComputed`, `IntoSignalF32`, `IntoText`, `IntoLabel`, `View`,
    /// `ViewBuilder` — whether the `.get()` is the argument itself or carried
    /// through `format!`, arithmetic, `.into()`, `as` casts, or struct-literal
    /// fields.
    ///
    /// ### Why is this bad?
    ///
    /// `.get()` reads the signal once and yields a plain value; the callee
    /// subscribes to nothing, so the view is frozen at the value it happened
    /// to read. `view.opacity(fade.get())` compiles and never animates.
    /// Handlers, `.map` closures, and tests are unaffected: the lint decides
    /// from the callee's signature, not from where the `.get()` textually
    /// sits.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// text("hello").opacity(fade.get())
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text("hello").opacity(fade.clone())
    /// ```
    pub SIGNAL_GET_IN_VIEW,
    correctness,
    "a `.get()` snapshot flows into a reactive or view parameter"
}

/// Def paths of the parameter bounds that expect a live signal or view.
/// Matched against the callee's generic predicates, so any call site whose
/// parameter carries one of these bounds is checked regardless of the crate
/// the callee lives in.
const REACTIVE_PARAM_BOUNDS: &[&[&str]] = &[
    &["nami", "reactive_core", "signal", "IntoSignal"],
    &["nami", "reactive_core", "signal", "IntoComputed"],
    &["waterui_core", "state", "computed_f32", "IntoSignalF32"],
    &["waterui_text", "text", "IntoText"],
    &["waterui_controls", "label", "IntoLabel"],
    &["waterui_core", "ui", "view", "View"],
    &["waterui_core", "foundation", "handler", "ViewBuilder"],
];

/// Def paths of the snapshot reads this lint tracks: the `Signal` trait's
/// `get` (reached through `Computed`, `Map`, `WithMetadata`, `SignalExt`
/// types, …) and `Binding`'s inherent `get`, which shadows the trait method
/// in method resolution.
const SNAPSHOT_GETS: &[&[&str]] = &[
    &["nami_core", "Signal", "get"],
    &["nami", "reactive_core", "binding", "Binding", "get"],
];

/// `text` — the only callee for which `text(format!(..))` can be rewritten as
/// `text!(..)`, since `text!` produces a `Text` directly.
const TEXT_FN: &[&str] = &["waterui_text", "text", "text"];

const MESSAGE: &str = "`.get()` reads the signal once; the view will never update";

const PASS_SIGNAL_HELP: &str = "pass the signal itself so the view can subscribe to updates";

pub(crate) struct SignalGetInView {
    format_args: FormatArgsStorage,
}

impl SignalGetInView {
    pub(crate) fn new(format_args: FormatArgsStorage) -> Self {
        Self { format_args }
    }
}

impl_lint_pass!(SignalGetInView => [SIGNAL_GET_IN_VIEW]);

/// A trait bound a callee's parameter carries, matched against
/// `REACTIVE_PARAM_BOUNDS`.
struct BoundTarget<'tcx> {
    trait_did: DefId,
    /// The bound's generic arguments minus `Self` (e.g. `[f32]` for
    /// `IntoComputed<f32>`), instantiated with the call site's substitutions.
    args: &'tcx [GenericArg<'tcx>],
}

/// `param index -> bounds on it that are in `REACTIVE_PARAM_BOUNDS``, for one
/// callee. Walks the `parent` chain so impl-level bounds (`impl<V: View>`) and
/// trait supertraits (`trait ViewExt: View`) are seen alongside the callee's
/// own `where`/APIT clauses.
fn reactive_param_bounds<'tcx>(
    cx: &LateContext<'tcx>,
    callee: DefId,
    substs: GenericArgsRef<'tcx>,
) -> FxHashMap<u32, Vec<BoundTarget<'tcx>>> {
    let mut bounds: FxHashMap<u32, Vec<BoundTarget<'tcx>>> = FxHashMap::default();
    for &(clause, _) in all_predicates_of(cx.tcx, callee) {
        let ClauseKind::Trait(pred) = clause.kind().skip_binder() else {
            continue;
        };
        if pred.polarity != PredicatePolarity::Positive
            || !REACTIVE_PARAM_BOUNDS
                .iter()
                .any(|path| crate::def_path::def_path_eq(cx, pred.trait_ref.def_id, path))
        {
            continue;
        }
        let TyKind::Param(param) = *pred.trait_ref.self_ty().kind() else {
            continue;
        };
        let instantiated = EarlyBinder::bind(clause).instantiate(cx.tcx, substs);
        let ClauseKind::Trait(inst_pred) = instantiated.kind().skip_norm_wip().skip_binder() else {
            continue;
        };
        bounds.entry(param.index).or_default().push(BoundTarget {
            trait_did: pred.trait_ref.def_id,
            args: &inst_pred.trait_ref.args[1..],
        });
    }
    bounds
}

/// The bound on `input` — a callee's declared parameter type — if it is a
/// type parameter bound by a reactive/view trait.
fn param_bounds<'a, 'tcx>(
    bounds: &'a FxHashMap<u32, Vec<BoundTarget<'tcx>>>,
    input: Ty<'tcx>,
) -> Option<&'a Vec<BoundTarget<'tcx>>> {
    let TyKind::Param(param) = *input.peel_refs().kind() else {
        return None;
    };
    bounds.get(&param.index)
}

/// Whether `expr` is a `x.get()` resolving to `Signal::get` or `Binding::get`.
///
/// A `computed.get()` call resolves to the method inside `impl Signal for
/// Computed`, whose def path is `<impl Signal for Computed>::get` — not the
/// trait's. `AssocContainer::TraitImpl` points back at the implemented trait
/// item, which normalizes those calls onto `nami_core::Signal::get`. The
/// inherent `Binding::get` keeps its own `InherentImpl` def path.
fn is_snapshot_get(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    let did = match expr.kind {
        ExprKind::MethodCall(..) => cx.typeck_results().type_dependent_def_id(expr.hir_id),
        ExprKind::Call(func, _) => match func.res(cx) {
            Res::Def(DefKind::AssocFn, did) => Some(did),
            _ => None,
        },
        _ => None,
    };
    let Some(did) = did else { return false };
    let did = match cx.tcx.associated_item(did).container {
        AssocContainer::TraitImpl(Ok(trait_item)) => trait_item,
        _ => did,
    };
    SNAPSHOT_GETS
        .iter()
        .any(|path| crate::def_path::def_path_eq(cx, did, path))
}

/// The receiver (`x` in `x.get()` / `Signal::get(x)`).
fn get_receiver<'tcx>(expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
    match expr.kind {
        ExprKind::MethodCall(_, receiver, [], _) => Some(receiver),
        ExprKind::Call(_, [receiver]) => Some(receiver),
        _ => None,
    }
}

/// Collects `.get()` snapshots inside one argument expression. Everything is
/// walked except closures and const blocks (deferred/foreign bodies where a
/// `.get()` is a legitimate read), nested items (skipped by `NestedFilter`),
/// and arguments that already sit in a reactive-bound position of a nested
/// call — those are reported by that call's own `check_expr`.
struct SnapshotGet<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    gets: Vec<&'tcx Expr<'tcx>>,
}

impl<'tcx> Visitor<'tcx> for SnapshotGet<'_, 'tcx> {
    type NestedFilter = nested_filter::None;

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        match expr.kind {
            ExprKind::Closure(_) | ExprKind::ConstBlock(_) => {}
            ExprKind::Call(func, args) => {
                if is_snapshot_get(self.cx, expr) {
                    self.gets.push(expr);
                }
                self.visit_expr(func);
                let mask = bound_arg_mask(self.cx, expr);
                for (index, arg) in args.iter().enumerate() {
                    if !mask.get(index).copied().unwrap_or_default() {
                        self.visit_expr(arg);
                    }
                }
            }
            ExprKind::MethodCall(_, receiver, args, _) => {
                if is_snapshot_get(self.cx, expr) {
                    self.gets.push(expr);
                }
                let mask = bound_arg_mask(self.cx, expr);
                for (index, arg) in std::iter::once(receiver).chain(args).enumerate() {
                    if !mask.get(index).copied().unwrap_or_default() {
                        self.visit_expr(arg);
                    }
                }
            }
            _ => intravisit::walk_expr(self, expr),
        }
    }
}

/// `mask[i]` — whether the `i`-th argument position of `call` (the receiver
/// counts as position 0 for method calls) maps to a reactive-bound parameter.
/// The inner `check_expr` owns diagnostics for those positions, so
/// [`SnapshotGet`] skips them to keep one diagnostic per `.get()`.
fn bound_arg_mask(cx: &LateContext<'_>, call: &Expr<'_>) -> Vec<bool> {
    let Some((callee, substs)) = fn_def_id_with_node_args(cx, call) else {
        return Vec::new();
    };
    let bounds = reactive_param_bounds(cx, callee, substs);
    if bounds.is_empty() {
        return Vec::new();
    }
    cx.tcx
        .fn_sig(callee)
        .instantiate_identity()
        .skip_norm_wip()
        .skip_binder()
        .inputs()
        .iter()
        .map(|input| param_bounds(&bounds, *input).is_some())
        .collect()
}

/// `Some(())` when `arg`, stripping drop-temps, is exactly `get`.
fn arg_is_the_get(arg: &Expr<'_>, get: &Expr<'_>) -> bool {
    let mut expr = arg;
    while let ExprKind::DropTemps(inner) = expr.kind {
        expr = inner;
    }
    expr.hir_id == get.hir_id
}

/// `x.clone()` when `x`'s type satisfies one of the parameter's bounds.
fn clone_suggestion<'tcx>(
    cx: &LateContext<'tcx>,
    get: &Expr<'tcx>,
    bounds: &[BoundTarget<'tcx>],
) -> Option<String> {
    let receiver = get_receiver(get)?;
    let receiver_ty = cx.typeck_results().expr_ty(receiver);
    let satisfies = bounds.iter().any(|target| {
        target.args.iter().all(|arg| !arg.has_infer())
            && implements_trait(cx, receiver_ty, target.trait_did, target.args)
    });
    if satisfies {
        Some(format!("{}.clone()", snippet_opt(cx, receiver.span)?))
    } else {
        None
    }
}

/// An identifier usable in a `text!` `{name}` placeholder, paired with the
/// source of the value it must be bound to — `None` when the name is a
/// plain in-scope binding that `text!` captures by itself.
fn placeholder_name(
    cx: &LateContext<'_>,
    fallback: usize,
    expr: &Expr<'_>,
) -> (String, Option<String>) {
    if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
        && let [segment] = path.segments
    {
        return (segment.ident.to_string(), None);
    }
    let name = match expr.kind {
        ExprKind::Field(_, ident) => ident.to_string(),
        ExprKind::Path(QPath::Resolved(None, path)) => path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_else(|| format!("arg{fallback}")),
        _ => format!("arg{fallback}"),
    };
    (
        name,
        Some(snippet_opt(cx, expr.span).unwrap_or_else(|| "_".into())),
    )
}

/// Builds the `text!("{name}"[, name = expr]*)` rewrite for
/// `text(format!(..))`, or `None` when the expansion or spans cannot be
/// mapped back to source.
fn text_macro_suggestion(
    storage: &FormatArgsStorage,
    cx: &LateContext<'_>,
    arg: &Expr<'_>,
) -> Option<String> {
    let macro_call = root_macro_call_first_node(cx, arg)?;
    if cx.tcx.get_diagnostic_name(macro_call.def_id) != Some(sym::format_macro) {
        return None;
    }
    let format_args = storage.get(cx, arg, macro_call.expn)?;

    // Name every argument; explicit `name = expr` bindings keep their name and
    // are re-emitted with the snapshot's receiver as the value.
    let mut names: Vec<String> = Vec::new();
    let mut bindings: Vec<String> = Vec::new();
    for (index, argument) in format_args.arguments.all_args().iter().enumerate() {
        let hir_arg = find_format_arg_expr(arg, argument);
        let get_receiver = hir_arg.and_then(|expr| {
            is_snapshot_get(cx, expr)
                .then(|| get_receiver(expr))
                .flatten()
        });
        let (name, binding) = match argument.kind {
            FormatArgumentKind::Captured(ident) => (ident.to_string(), None),
            FormatArgumentKind::Named(ident) => {
                let source = get_receiver
                    .or(hir_arg)
                    .and_then(|expr| snippet_opt(cx, expr.span))?;
                (ident.to_string(), Some(source))
            }
            FormatArgumentKind::Normal => placeholder_name(cx, index, get_receiver.or(hir_arg)?),
        };
        if let Some(source) = binding {
            bindings.push(format!("{name} = {source}"));
        }
        names.push(name);
    }

    // Rebuild the literal from the template: every placeholder becomes
    // `{name}` since `text!` only takes named slots. Template literals are
    // unescaped text, so braces they contain must be re-escaped as `{{`/`}}`.
    let mut literal = String::new();
    for piece in &format_args.template {
        match piece {
            FormatArgsPiece::Literal(text) => {
                for ch in text.as_str().chars() {
                    match ch {
                        '{' => literal.push_str("{{"),
                        '}' => literal.push_str("}}"),
                        _ => literal.push(ch),
                    }
                }
            }
            FormatArgsPiece::Placeholder(placeholder) => {
                let index = placeholder.argument.index.ok()?;
                literal.push('{');
                literal.push_str(names.get(index)?);
                literal.push('}');
            }
        }
    }

    let mut suggestion = format!("text!({literal:?}");
    for binding in &bindings {
        suggestion.push_str(", ");
        suggestion.push_str(binding);
    }
    suggestion.push(')');
    Some(suggestion)
}

fn report_arg<'tcx>(
    cx: &LateContext<'tcx>,
    storage: &FormatArgsStorage,
    call: &'tcx Expr<'tcx>,
    callee: DefId,
    arg: &'tcx Expr<'tcx>,
    bounds: &[BoundTarget<'tcx>],
) {
    let mut finder = SnapshotGet {
        cx,
        gets: Vec::new(),
    };
    finder.visit_expr(arg);
    for (index, get) in finder.gets.iter().copied().enumerate() {
        // A `.get()` a macro wrote (`text!`'s own subscription plumbing) is
        // not the user's; `format!` arguments keep their call-site spans, so
        // the `text(format!(.., x.get()))` case is unaffected.
        if get.span.from_expansion() {
            continue;
        }
        if arg_is_the_get(arg, get) {
            match clone_suggestion(cx, get, bounds) {
                Some(suggestion) => span_lint_and_sugg(
                    cx,
                    SIGNAL_GET_IN_VIEW,
                    get.span,
                    MESSAGE,
                    PASS_SIGNAL_HELP,
                    suggestion,
                    Applicability::MachineApplicable,
                ),
                None => {
                    span_lint_and_help(
                        cx,
                        SIGNAL_GET_IN_VIEW,
                        get.span,
                        MESSAGE,
                        None,
                        PASS_SIGNAL_HELP,
                    );
                }
            }
        } else if index == 0 && crate::def_path::def_path_eq(cx, callee, TEXT_FN) {
            match text_macro_suggestion(storage, cx, arg) {
                Some(suggestion) => {
                    span_lint_and_then(cx, SIGNAL_GET_IN_VIEW, get.span, MESSAGE, |diag| {
                        diag.span_suggestion(
                            call.span,
                            "use `text!` so the placeholders subscribe to the signals",
                            suggestion,
                            Applicability::MaybeIncorrect,
                        );
                    })
                }
                None => {
                    span_lint_and_help(
                        cx,
                        SIGNAL_GET_IN_VIEW,
                        get.span,
                        MESSAGE,
                        None,
                        PASS_SIGNAL_HELP,
                    );
                }
            }
        } else {
            span_lint_and_help(
                cx,
                SIGNAL_GET_IN_VIEW,
                get.span,
                MESSAGE,
                None,
                PASS_SIGNAL_HELP,
            );
        }
    }
}

impl<'tcx> LateLintPass<'tcx> for SignalGetInView {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        let Some((callee, substs)) = fn_def_id_with_node_args(cx, expr) else {
            return;
        };
        let bounds = reactive_param_bounds(cx, callee, substs);
        if bounds.is_empty() {
            return;
        }
        let sig = cx
            .tcx
            .fn_sig(callee)
            .instantiate_identity()
            .skip_norm_wip()
            .skip_binder();
        let args = match expr.kind {
            ExprKind::Call(_, args) => args.iter().collect::<Vec<_>>(),
            ExprKind::MethodCall(_, receiver, args, _) => {
                std::iter::once(receiver).chain(args).collect()
            }
            _ => return,
        };
        for (arg, input) in args.into_iter().zip(sig.inputs()) {
            if let Some(targets) = param_bounds(&bounds, *input) {
                report_arg(cx, &self.format_args, expr, callee, arg, targets);
            }
        }
    }
}

/// Early pass mirroring clippy's `FormatArgsCollector`: snapshots the AST
/// `FormatArgs` nodes so the late pass can rebuild a `format!(..)` argument as
/// a `text!` invocation — the desugared HIR no longer carries the literal's
/// placeholders.
pub(crate) struct FormatArgsCollector {
    format_args: FxHashMap<Span, FormatArgs>,
    storage: FormatArgsStorage,
}

impl FormatArgsCollector {
    pub(crate) fn new(storage: FormatArgsStorage) -> Self {
        Self {
            format_args: FxHashMap::default(),
            storage,
        }
    }
}

impl_lint_pass!(FormatArgsCollector => []);

impl EarlyLintPass for FormatArgsCollector {
    fn check_expr(&mut self, cx: &EarlyContext<'_>, expr: &AstExpr) {
        if let AstExprKind::FormatArgs(args) = &expr.kind {
            if has_span_from_proc_macro(cx, args) {
                return;
            }

            self.format_args
                .insert(expr.span.with_parent(None), (**args).clone());
        }
    }

    fn check_crate_post(&mut self, _: &EarlyContext<'_>, _: &AstCrate) {
        self.storage.set(mem::take(&mut self.format_args));
    }
}

/// Detects if the format string or an argument has its span set by a proc
/// macro to something inside a macro callsite (clippy's guard of the same
/// name — e.g. `println!(some_proc_macro!("input {}"), a)`).
fn has_span_from_proc_macro(cx: &EarlyContext<'_>, args: &FormatArgs) -> bool {
    let ctxt = args.span.ctxt();

    let mut spans = std::iter::once(args.span).chain(
        args.arguments
            .explicit_args()
            .iter()
            .map(|argument| hygiene::walk_chain(argument.expr.span, ctxt)),
    );
    let Some(mut start) = spans.next() else {
        return false;
    };
    for end in spans {
        if !start
            .between(end)
            .check_source_text(cx, between_args_is_comma_name)
        {
            return true;
        }
        start = end;
    }
    false
}

/// The source between two consecutive `format!` inputs must be `, name` or
/// `, name =` — anything else means the spans were mapped by a proc macro.
fn between_args_is_comma_name(src: &str) -> bool {
    let mut tokens = tokenize(src, FrontmatterAllowed::No).filter(|token| {
        !matches!(
            token.kind,
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } | TokenKind::Whitespace
        )
    });
    tokens
        .next()
        .is_some_and(|t| matches!(t.kind, TokenKind::Comma))
        && tokens.all(|t| matches!(t.kind, TokenKind::Ident | TokenKind::Eq))
}
