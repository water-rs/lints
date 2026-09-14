//! `manual_text_map` — `.map(|v| format!(..))` over a signal whose result
//! only feeds a text position; `text!` formats the signal itself.

use clippy_utils::diagnostics::span_lint_and_then;
use clippy_utils::macros::{FormatArgsStorage, find_format_arg_expr, root_macro_call_first_node};
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::source::snippet_opt;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::for_each_expr_without_closures;
use rustc_ast::format::{
    FormatAlignment, FormatArgsPiece, FormatCount, FormatDebugHex, FormatOptions, FormatSign,
    FormatTrait,
};
use rustc_ast::token;
use rustc_ast::{FormatArgs, LitKind};
use rustc_data_structures::fx::{FxHashMap, FxHashSet};
use rustc_errors::Applicability;
use rustc_hir::def::{Namespace, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::intravisit::{self, Visitor};
use rustc_hir::{Block, Expr, ExprKind, HirId, LetStmt, Node, Pat, PatKind, QPath};
use rustc_lexer::{FrontmatterAllowed, LiteralKind, TokenKind, tokenize};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{Ty, TyCtxt, TyKind, TypeVisitableExt, TypeckResults};
use rustc_session::impl_lint_pass;
use rustc_span::symbol::{Ident, Symbol};
use rustc_span::{ExpnKind, MacroKind, Span, sym};
use std::cell::OnceCell;
use std::ops::{ControlFlow, Range};

use crate::carriers::{CLONE, TO_OWNED, TO_STRING, is_call_to, strip_wraps};
use crate::def_path::def_path_eq;
use crate::imports::{Bare, bare_status, use_insertion};
use crate::param_bounds::{call_arg_bounds_in, call_args};
use crate::snapshot_get::{get_receiver, is_snapshot_get_in};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags `.map(|v| format!(..))`-style signal transforms — `format!`,
    /// `v.to_string()`, `v.clone()` — whose mapped string only ever reaches a
    /// text position: a `text(..)` call, a parameter bounded by `IntoText` or
    /// `IntoLabel`, a `text!` placeholder, or a `let` binding that feeds one
    /// of those. `zip(a, b).map(|(x, y)| format!(..))` is covered the same
    /// way, and `.clone()`/`&`-wrappers on the receiver are stripped.
    ///
    /// ### Why is this bad?
    ///
    /// `text!` formats signals itself and keeps the format template, so the
    /// string re-resolves when the locale changes and stays one translatable
    /// sentence. `format!` inside `map` produces a `String` eagerly: the
    /// template is thrown away, format specs are fixed at build time, and the
    /// sentence is split between the map and the surrounding text.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let selection_text = selection.clone().map(|fruit| format!("{fruit:?}"));
    /// hstack(("Selected: ", text!("{selection_text}")))
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// text!("Selected: {selection:?}")
    /// ```
    pub MANUAL_TEXT_MAP,
    style,
    "`text!` already formats a signal — `.map(|v| format!(..))` throws away format specs and the translatable sentence"
}

const MESSAGE: &str = "`text!` already formats a signal — `.map(|v| format!(..))` throws away \
     format specs and the translatable sentence";

/// `SignalExt::map` and `nami::reactive_core::map::map` — the two spellings
/// of "transform this signal through a closure".
const MAP_FNS: &[&[&str]] = &[
    &["nami", "reactive_core", "ext", "SignalExt", "map"],
    &["nami", "reactive_core", "map", "map"],
];

/// `SignalExt::zip` and `nami::zip` — both splice several signals into one;
/// the lint flattens them into the map's signal sources.
const ZIP_FNS: &[&[&str]] = &[
    &["nami", "reactive_core", "ext", "SignalExt", "zip"],
    &["nami", "reactive_core", "zip", "zip"],
];

/// `From::from` — `String::from(format!(..))` / `Str::from(v)` wrappers.
const FROM: &[&str] = &["core", "convert", "From", "from"];

/// `Into::into` — `format!(..).into()`.
const INTO: &[&str] = &["core", "convert", "Into", "into"];

/// Calls a map result may pass through on its way to a text position. They
/// only rewrap the signal, so the analysis peels them and the fix drops them.
const RESULT_ADAPTERS: &[&[&str]] = &[
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

/// `waterui_text::text::text` — the `text(..)` function; the only callee for
/// which the whole call is replaced by `text!(..)`.
const TEXT_FN: &[&str] = &["waterui_text", "text", "text"];

/// `Text::localized_with` — the call `text!` emits around its captures; an
/// ancestor of it marks an expression as living inside the expansion.
const LOCALIZED_WITH: &[&str] = &["waterui_text", "text", "Text", "localized_with"];

/// Parameter bounds that take a text-producing value.
const TEXT_BOUNDS: &[&[&str]] = &[
    &["waterui_text", "text", "IntoText"],
    &["waterui_controls", "label", "IntoLabel"],
];

/// String-shaped types a mapped closure may produce.
const STRING_TYS: &[&[&str]] = &[&["alloc", "string", "String"], &["waterui_str", "Str"]];

/// `use` text inserted when `text` isn't already in scope.
const TEXT_USE: &str = "waterui::text";

/// A candidate `text!` slot name must be a plain identifier — `text!`
/// captures slots by name and parses `name = expr` bindings.
fn usable_ident(ident: Ident) -> Option<Symbol> {
    (!ident.is_reserved() && !ident.is_special()).then_some(ident.name)
}

/// `expr` is a bare identifier path (`count`, `selection`) — the shape a
/// `text!` slot can capture by name. `self.count` has two segments and does
/// not qualify.
fn bare_path_ident(expr: &Expr<'_>) -> Option<Symbol> {
    if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
        && let [segment] = path.segments
    {
        return usable_ident(segment.ident);
    }
    None
}

/// `sig.map(f)` / `nami::map(sig, f)` → `(receiver, closure)`.
fn map_call<'hir>(
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
fn closure_parts<'hir>(
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

/// The signals the map reads: `zip(a, b)` / `a.zip(&b)` flatten to their
/// arguments (recursively, for nested zips); anything else is the single
/// source.
fn sources<'hir>(
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

/// `String` / `Str` / `&str` — the string-shaped output a text-feeding map
/// must produce.
fn is_string_ty(cx: &LateContext<'_>, ty: Ty<'_>) -> bool {
    match ty.peel_refs().kind() {
        TyKind::Str => true,
        TyKind::Adt(def, _) => STRING_TYS.iter().any(|p| def_path_eq(cx, def.did(), p)),
        _ => false,
    }
}

/// The bindings a map-closure parameter pattern makes, related to the
/// signal sources it destructures.
struct Params {
    /// Whole-source bindings — `v` in `|v|`, `x`/`y` in `|(x, y)|` over
    /// `zip(a, b)` — binding `HirId` → source index.
    source_of: FxHashMap<HirId, usize>,
    /// Every binding the pattern makes, partial destructures included.
    all: FxHashSet<HirId>,
    /// Binding name per source position — the `text!` slot name a non-bare
    /// receiver aliases to.
    names: Vec<Option<Symbol>>,
    /// The pattern's own span — reused in the help alias.
    span: Span,
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
    fn new(pat: &Pat<'_>, source_count: usize) -> Self {
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
fn param_hir_id(expr: &Expr<'_>, params: &FxHashSet<HirId>) -> Option<HirId> {
    if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
        && let Res::Local(hir_id) = path.res
        && params.contains(&hir_id)
    {
        return Some(hir_id);
    }
    None
}

/// Whether `expr` mentions any of `params`, not descending into closures.
fn touches_param(expr: &Expr<'_>, params: &FxHashSet<HirId>) -> bool {
    for_each_expr_without_closures(expr, |e| {
        if param_hir_id(e, params).is_some() {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_some()
}

/// `format!(..).into()`, `String::from(format!(..))`, `{ format!(..) }` —
/// tail conversions/blocks around a mapped string are transparent to this
/// lint.
fn strip_tail<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    mut expr: &'hir Expr<'hir>,
) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::DropTemps(inner) => inner,
            ExprKind::Block(
                Block {
                    stmts: [],
                    expr: Some(inner),
                    ..
                },
                _,
            ) => inner,
            _ if is_call_to(cx, typeck, expr, &[INTO, FROM])
                && is_string_ty(cx, typeck.expr_ty(expr)) =>
            {
                match call_args(expr).as_slice() {
                    [first, ..] => *first,
                    [] => return expr,
                }
            }
            _ => return expr,
        };
    }
}

/// The two `TypeckResults` the lint consults: the body holding the `map`
/// call and the closure's own body, which is a nested body with a table of
/// its own.
#[derive(Clone, Copy)]
struct Typing<'tcx> {
    /// The body containing the `map` call.
    call: &'tcx TypeckResults<'tcx>,
    /// The map closure's body.
    closure: &'tcx TypeckResults<'tcx>,
}

/// A flagged `map` call, fully analyzed — everything `report` needs.
struct Flag<'a, 'hir> {
    /// The `map` call expr — the lint's primary span.
    expr: &'hir Expr<'hir>,
    /// The map receiver (before `zip`-flattening) — the alias help renders
    /// `<receiver>.map(|<pat>| <expr>)` from it.
    receiver: &'hir Expr<'hir>,
    /// The closure pattern's bindings.
    params: &'a Params,
    /// What the closure body produces.
    mapped: &'a Mapped<'a, 'hir>,
    /// Where the mapped result flows.
    position: Position<'hir>,
}

/// What the map's closure body does with its parameter.
enum Mapped<'a, 'hir> {
    /// `format!(..)` — the AST template survived in the early-pass storage;
    /// the HIR call is kept to map arguments back to expressions.
    Format(&'a FormatArgs, &'hir Expr<'hir>),
    /// `v.to_string()` / `v.clone()` over a string — the body renders the
    /// parameter. The payload is the conversion's receiver.
    Direct(&'hir Expr<'hir>),
    /// A string conversion over a non-bare projection of a parameter —
    /// `u.name.to_string()` — flagged, but a `text!` slot can't name it.
    Projected(&'hir Expr<'hir>),
    /// Nothing text-shaped — the `format!` never touches the parameter, or
    /// the body isn't a string at all.
    Silent,
}

/// How a `format!` argument relates to the map's signal sources.
#[derive(Clone, Copy)]
enum ArgClass {
    /// A bare closure-param binding — payload is the signal-source index.
    Param(usize),
    /// A bare in-scope `Signal`-typed name (`total`, `total.get()`) — `text!`
    /// captures it as-is.
    Capture(Symbol),
    /// Touches a closure param without being a bare binding — `u.name`. The
    /// fix aliases `<receiver>.map(|<pat>| <expr>)`, which stays a signal.
    Projected,
    /// A `Signal`-typed expression that isn't a bare ident — `sigs[0]`,
    /// `x.computed()`. The fix aliases it under a name.
    ForeignSignal,
    /// Not a signal at all — `text!` can't subscribe to it. The fix binds it
    /// as `constant(<expr>)`.
    ForeignValue,
}

/// Where the map's result flows.
#[derive(Clone, Copy)]
enum Position<'hir> {
    /// `text(<arg>)` — the fix replaces the whole call.
    TextCall(&'hir Expr<'hir>),
    /// `callee(<arg>)` where the parameter is `IntoText`/`IntoLabel` — the
    /// fix replaces the argument only.
    BoundArg(&'hir Expr<'hir>),
    /// The map feeds a `text!` slot through `text!("..{slot}..", slot = <map>)`
    /// — `call_site` is the `text!(..)` invocation, `slot` its alias name.
    TextMacro {
        call_site: Span,
        slot: Option<Symbol>,
    },
    /// `let <name> = <map>` with `<name>` reaching a text position.
    LetBound { name: Symbol, text_use: TextUse },
    /// Nowhere text-shaped.
    Silent,
}

/// A use of a `let`-bound map that lands in text.
#[derive(Clone, Copy)]
enum TextUse {
    /// The use is a `text!` capture — `call_site` is the invocation for the
    /// merge, `slot` the capture's name.
    Macro {
        call_site: Span,
        slot: Option<Symbol>,
    },
    /// The use's peeled value is an argument of a text-position call.
    Arg,
}

/// The result of walking an expression's parents: either the first
/// non-transparent consumer, a `let` initializer, or a `text!` capture.
enum Flow<'hir> {
    /// The peeled expression ended as an argument of `call`.
    Arg {
        call: &'hir Expr<'hir>,
        arg: &'hir Expr<'hir>,
    },
    /// The peeled expression is `local`'s initializer.
    LetInit(&'hir LetStmt<'hir>),
    /// The peeled expression is a `text!` capture binding's value.
    TextMacro {
        call_site: Span,
        slot: Option<Symbol>,
    },
    /// The expression is consumed by something else.
    End,
}

/// Whether `local` is one of `text!`'s capture bindings — the macro emits
/// `let <name> = (<expr>).to_owned()` inside the `Text::localized_with` call
/// that carries them. Both halves are checked: the let's own span sits in a
/// bang-macro expansion named `text` (or whose `macro_def_id` is
/// `waterui_macros::text`, covering `use .. as ..` aliases), and an ancestor
/// is the `localized_with` call. A hand-written `localized_with` block that
/// happens to `.to_owned()` a map does not qualify — its `let` carries no
/// expansion mark.
fn inside_text_expansion<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    local: &LetStmt<'hir>,
    text: Option<DefId>,
) -> bool {
    let expn = local.span.ctxt().outer_expn_data();
    let marked = match expn.kind {
        ExpnKind::Macro(MacroKind::Bang, name) => {
            name == Symbol::intern("text") || text.is_some_and(|did| expn.macro_def_id == Some(did))
        }
        _ => false,
    };
    marked
        && cx
            .tcx
            .hir_parent_iter(local.hir_id)
            .any(|(_, node)| match node {
                Node::Expr(expr) => {
                    matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
                        && is_call_to(cx, typeck, expr, &[LOCALIZED_WITH])
                }
                _ => false,
            })
}

/// The name a `let` pattern binds (`let <ident> = ..`).
fn binding_name(pat: &Pat<'_>) -> Option<Symbol> {
    if let PatKind::Binding(_, _, ident, _) = pat.kind {
        usable_ident(ident)
    } else {
        None
    }
}

/// Walk `expr`'s parents: adapter calls and transparent wrappers peel, a
/// `Call`/`MethodCall` argument position reports `Arg`, a `let` initializer
/// reports `LetInit` (or `TextMacro` for `text!`'s `.to_owned()` capture),
/// anything else stops the walk.
fn flow<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
    text: Option<DefId>,
) -> Flow<'hir> {
    let mut current = expr;
    for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
        match node {
            Node::Expr(parent) => match parent.kind {
                ExprKind::DropTemps(inner) if inner.hir_id == current.hir_id => current = parent,
                ExprKind::Block(
                    Block {
                        stmts: [],
                        expr: Some(tail),
                        ..
                    },
                    _,
                ) if tail.hir_id == current.hir_id => {
                    current = parent;
                }
                ExprKind::If(_, then, else_)
                    if then.hir_id == current.hir_id
                        || else_.is_some_and(|e| e.hir_id == current.hir_id) =>
                {
                    current = parent;
                }
                ExprKind::Match(_, arms, _)
                    if arms.iter().any(|arm| arm.body.hir_id == current.hir_id) =>
                {
                    current = parent;
                }
                ExprKind::MethodCall(..) | ExprKind::Call(..) => {
                    let args = call_args(parent);
                    let Some(position) = args.iter().position(|a| a.hir_id == current.hir_id)
                    else {
                        // `current` is the callee, not an operand.
                        return Flow::End;
                    };
                    if position == 0
                        && (is_call_to(cx, typeck, parent, &[TO_OWNED, CLONE])
                            || is_call_to(cx, typeck, parent, RESULT_ADAPTERS))
                    {
                        current = parent;
                    } else {
                        return Flow::Arg {
                            call: parent,
                            arg: current,
                        };
                    }
                }
                _ => return Flow::End,
            },
            Node::LetStmt(local)
                if local.init.is_some_and(|init| init.hir_id == current.hir_id) =>
            {
                if matches!(current.kind, ExprKind::MethodCall(..) | ExprKind::Call(..))
                    && is_call_to(cx, typeck, current, &[TO_OWNED])
                    && inside_text_expansion(cx, typeck, local, text)
                {
                    return Flow::TextMacro {
                        call_site: local.span,
                        slot: binding_name(local.pat),
                    };
                }
                return Flow::LetInit(local);
            }
            Node::Arm(arm) if arm.body.hir_id == current.hir_id => {}
            _ => return Flow::End,
        }
    }
    Flow::End
}

/// Whether `arg` in `call` lands on an `IntoText`/`IntoLabel` parameter.
enum TextPos {
    /// The callee is `text` itself — the whole call is replaced.
    WholeCall,
    /// Another callee's bound parameter — only the argument is replaced.
    Arg,
    /// Not a text position.
    None,
}

fn text_position<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    call: &Expr<'hir>,
    arg: &Expr<'hir>,
) -> TextPos {
    let Some((callee, per_arg)) = call_arg_bounds_in(cx, typeck, call, &[TEXT_BOUNDS]) else {
        return TextPos::None;
    };
    let Some(index) = call_args(call).iter().position(|a| a.hir_id == arg.hir_id) else {
        return TextPos::None;
    };
    if per_arg.get(index).is_none_or(|targets| targets.is_empty()) {
        return TextPos::None;
    }
    if def_path_eq(cx, callee, TEXT_FN) {
        TextPos::WholeCall
    } else {
        TextPos::Arg
    }
}

/// Collects every read of one `let` binding in a block, in source order.
/// Closures are foreign bodies — a use inside one still reports its `flow`
/// against that body's `TypeckResults`, which `let_position` computes per
/// use.
struct LocalUses {
    pat: HirId,
    uses: Vec<HirId>,
}

impl<'tcx> Visitor<'tcx> for LocalUses {
    type NestedFilter = intravisit::nested_filter::None;

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        match expr.kind {
            ExprKind::Closure(_) | ExprKind::ConstBlock(_) => {}
            _ => {
                if let ExprKind::Path(QPath::Resolved(None, path)) = expr.kind
                    && path.res == Res::Local(self.pat)
                {
                    self.uses.push(expr.hir_id);
                }
                intravisit::walk_expr(self, expr);
            }
        }
    }
}

/// A `text!("lit" @ ctx, name = expr, ..)` invocation's source, parsed for
/// the merge — `literal` is the template's cooked text.
struct TextCall {
    /// The format literal's cooked value.
    literal: String,
    /// The `@ "ctx"` / `@ ident` clause, verbatim source to re-emit.
    context: Option<String>,
    /// `(name, value source range)` — the `name = expr` pairs.
    bindings: Vec<(String, Range<usize>)>,
}

/// The non-trivia tokens of `src` with their byte ranges.
fn tokens(src: &str) -> Vec<(TokenKind, Range<usize>)> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    for token in tokenize(src, FrontmatterAllowed::No) {
        let start = pos;
        pos += token.len as usize;
        if !matches!(
            token.kind,
            TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } | TokenKind::Whitespace
        ) {
            out.push((token.kind, start..pos));
        }
    }
    out
}

/// Unescape a `".."` / `r#".."#` literal to its cooked value.
fn unescape_str(text: &str) -> Option<String> {
    let (kind, content) = if let Some(rest) = text.strip_prefix('r') {
        let hashes = rest.bytes().take_while(|&b| b == b'#').count();
        // `r###"content"###` — strip the `r`, the hashes, and both quotes.
        let content = rest.get(hashes + 1..rest.len() - hashes - 1)?;
        (token::LitKind::StrRaw(hashes as u8), content)
    } else {
        (
            token::LitKind::Str,
            text.strip_prefix('"')?.strip_suffix('"')?,
        )
    };
    let lit = LitKind::from_token_lit(token::Lit::new(kind, Symbol::intern(content), None)).ok()?;
    if let LitKind::Str(symbol, _) = lit {
        Some(symbol.to_string())
    } else {
        None
    }
}

/// Parse `text!("lit" @ ctx, name = expr, ..)` back into parts — `None` when
/// the source isn't that shape.
fn parse_text_call(src: &str) -> Option<TextCall> {
    let tokens = tokens(src);
    // `text ! ( ... )` — find the bang, then the opening delimiter.
    let bang = tokens
        .iter()
        .position(|(kind, _)| matches!(kind, TokenKind::Bang))?;
    if !matches!(
        tokens.get(bang + 1)?.0,
        TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket
    ) {
        return None;
    }
    let mut literal = None;
    let mut context = None;
    let mut bindings = Vec::new();
    let mut index = bang + 2;
    while index < tokens.len() {
        let mut depth = 1usize;
        // Skip nested delimiters wholesale — the interesting tokens live at
        // depth 1.
        let (kind, range) = &tokens[index];
        match kind {
            TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => {
                index += 1;
                while index < tokens.len() && depth > 0 {
                    match tokens[index].0 {
                        TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => {
                            depth += 1;
                        }
                        TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => {
                            depth -= 1
                        }
                        _ => {}
                    }
                    index += 1;
                }
                continue;
            }
            TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => break,
            TokenKind::Literal {
                kind:
                    LiteralKind::Str { terminated: true } | LiteralKind::RawStr { n_hashes: Some(_) },
                ..
            } if literal.is_none() => {
                literal = Some(unescape_str(&src[range.clone()])?);
            }
            TokenKind::At if literal.is_some() && context.is_none() => {
                // `@ "ctx"` or `@ ident` — the next token is the context.
                let ctx_range = tokens.get(index + 1)?.1.clone();
                context = Some(src[range.start..ctx_range.end].to_owned());
                index += 1;
            }
            TokenKind::Ident if matches!(tokens.get(index + 1), Some((TokenKind::Eq, _))) => {
                // `name = <expr>` — the value runs to the next depth-1 comma
                // or the closing delimiter.
                let name = src[range.clone()].to_owned();
                let value_start = tokens.get(index + 2)?.1.start;
                let mut end = index + 2;
                let mut nested = 0usize;
                while let Some((kind, _)) = tokens.get(end) {
                    match kind {
                        TokenKind::OpenParen | TokenKind::OpenBrace | TokenKind::OpenBracket => {
                            nested += 1;
                        }
                        TokenKind::CloseParen | TokenKind::CloseBrace | TokenKind::CloseBracket => {
                            if nested == 0 {
                                break;
                            }
                            nested -= 1;
                        }
                        TokenKind::Comma if nested == 0 => break,
                        _ => {}
                    }
                    end += 1;
                }
                if end == index + 2 {
                    return None;
                }
                let value_end = tokens[end - 1].1.end;
                bindings.push((name, value_start..value_end));
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    Some(TextCall {
        literal: literal?,
        context,
        bindings,
    })
}

/// Escape `text` for reuse inside a `text!` literal — braces double up, the
/// rest is verbatim.
fn escape_literal(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '{' => out.push_str("{{"),
            '}' => out.push_str("}}"),
            _ => out.push(ch),
        }
    }
}

/// `:?`/`:>8.2`/`:x?` — everything `FormatOptions`/`FormatTrait` carries,
/// returned with the leading `:`, or `None` when a spec uses an argument
/// count (`{:.*}`, `{:w$}`), which a `text!` rewrite cannot carry.
fn render_options(options: &FormatOptions, trait_: FormatTrait) -> Option<String> {
    for count in [options.width.as_ref(), options.precision.as_ref()]
        .into_iter()
        .flatten()
    {
        if matches!(count, FormatCount::Argument(_)) {
            return None;
        }
    }
    let mut spec = String::new();
    if let Some(alignment) = options.alignment {
        if let Some(fill) = options.fill {
            spec.push(fill);
        }
        spec.push(match alignment {
            FormatAlignment::Left => '<',
            FormatAlignment::Right => '>',
            FormatAlignment::Center => '^',
        });
    }
    if let Some(sign) = options.sign {
        spec.push(match sign {
            FormatSign::Plus => '+',
            FormatSign::Minus => '-',
        });
    }
    if options.alternate {
        spec.push('#');
    }
    if options.zero_pad {
        spec.push('0');
    }
    if let Some(FormatCount::Literal(width)) = &options.width {
        spec.push_str(&width.to_string());
    }
    if let Some(FormatCount::Literal(precision)) = &options.precision {
        spec.push('.');
        spec.push_str(&precision.to_string());
    }
    if let Some(debug_hex) = options.debug_hex {
        spec.push(match debug_hex {
            FormatDebugHex::Lower => 'x',
            FormatDebugHex::Upper => 'X',
        });
    }
    spec.push_str(match trait_ {
        FormatTrait::Display => "",
        FormatTrait::Debug => "?",
        FormatTrait::LowerExp => "e",
        FormatTrait::UpperExp => "E",
        FormatTrait::Octal => "o",
        FormatTrait::Pointer => "p",
        FormatTrait::Binary => "b",
        FormatTrait::LowerHex => "x",
        FormatTrait::UpperHex => "X",
    });
    Some(if spec.is_empty() {
        spec
    } else {
        format!(":{spec}")
    })
}

/// What the fix can build.
enum Render {
    /// A `text!` call — literal plus `name = expr` bindings.
    Text(Rendered),
    /// The shape can't rebuild `text!` — this is the `help` text.
    Help(String),
    /// The closure body never formats a signal — not this lint.
    Silent,
}

/// A rebuilt `text!` invocation's payload.
struct Rendered {
    /// The literal contents — `n = {v:?}`.
    template: String,
    /// `name = expr` bindings appended after the literal.
    bindings: Vec<String>,
    /// Every used source was a bare identifier — `MachineApplicable`.
    all_bare: bool,
}

impl Rendered {
    /// `text!("{template}"[, binding]*)`.
    fn text_call(&self) -> String {
        let mut out = format!("text!({:?}", self.template);
        for binding in &self.bindings {
            out.push_str(", ");
            out.push_str(binding);
        }
        out.push(')');
        out
    }
}

/// Resolves `text!` slot names for the sources — bare identifiers capture by
/// name; anything else keeps the parameter's name and gains a
/// `name = <source>` binding.
struct Namer<'hir> {
    sources: Vec<&'hir Expr<'hir>>,
    names: Vec<Option<Symbol>>,
    /// Resolved slot per source, filled on first use.
    slots: Vec<Option<String>>,
    bindings: Vec<String>,
    all_bare: bool,
}

impl<'hir> Namer<'hir> {
    fn new(sources: Vec<&'hir Expr<'hir>>, params: &Params) -> Self {
        Self {
            slots: vec![None; sources.len()],
            names: params.names.clone(),
            sources,
            bindings: Vec::new(),
            all_bare: true,
        }
    }

    /// The `text!` slot name for source `i` — `None` when the source's
    /// snippet can't be recovered for the alias binding.
    fn slot(&mut self, cx: &LateContext<'_>, index: usize) -> Option<&str> {
        if self.slots[index].is_none() {
            let name = match bare_path_ident(self.sources[index]) {
                Some(name) => name.to_string(),
                None => {
                    self.all_bare = false;
                    let name = self
                        .names
                        .get(index)
                        .copied()
                        .flatten()
                        .map(|name| name.to_string())
                        .unwrap_or_else(|| format!("arg{index}"));
                    let source = snippet_opt(cx, self.sources[index].span)?;
                    self.bindings.push(format!("{name} = {source}"));
                    name
                }
            };
            self.slots[index] = Some(name);
        }
        self.slots[index].as_deref()
    }
}

/// A name for an alias binding derived from the expression itself —
/// `u.name` → `name`, `f(x)` → `arg{index}`.
fn alias_name(expr: &Expr<'_>, index: usize) -> String {
    match expr.kind {
        ExprKind::Field(_, ident) => ident.to_string(),
        ExprKind::Path(QPath::Resolved(None, path)) => path
            .segments
            .last()
            .and_then(|segment| usable_ident(segment.ident))
            .map(|name| name.to_string())
            .unwrap_or_else(|| format!("arg{index}")),
        _ => format!("arg{index}"),
    }
}

pub(crate) struct ManualTextMap {
    format_args: FormatArgsStorage,
    text_macro: OnceCell<Option<DefId>>,
    signal: OnceCell<Option<DefId>>,
}

impl_lint_pass!(ManualTextMap => [MANUAL_TEXT_MAP]);

impl ManualTextMap {
    pub(crate) fn new(format_args: FormatArgsStorage) -> Self {
        Self {
            format_args,
            text_macro: OnceCell::new(),
            signal: OnceCell::new(),
        }
    }

    /// `waterui_macros::text`'s `DefId` — the target `bare_status` resolves
    /// `text` against.
    fn text_macro(&self, cx: &LateContext<'_>) -> Option<DefId> {
        *self.text_macro.get_or_init(|| {
            lookup_path_str(cx.tcx, PathNS::Macro, "waterui_macros::text")
                .first()
                .copied()
        })
    }

    /// `nami_core::Signal`'s `DefId` — for `Capture` classification.
    fn signal(&self, cx: &LateContext<'_>) -> Option<DefId> {
        *self.signal.get_or_init(|| {
            lookup_path_str(cx.tcx, PathNS::Type, "nami_core::Signal")
                .first()
                .copied()
        })
    }

    /// Whether `expr` is a `Signal`-typed value.
    fn is_signal<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        typeck: &TypeckResults<'tcx>,
        expr: &Expr<'tcx>,
    ) -> bool {
        let Some(signal) = self.signal(cx) else {
            return false;
        };
        let ty = typeck.expr_ty(expr);
        !ty.has_infer() && implements_trait(cx, ty, signal, &[])
    }

    /// Classify the closure body — what it produces from the parameter.
    fn classify_body<'a, 'hir>(
        &'a self,
        cx: &LateContext<'hir>,
        typeck: &TypeckResults<'hir>,
        body: &'hir Expr<'hir>,
        params: &Params,
    ) -> Mapped<'a, 'hir> {
        let body = strip_tail(cx, typeck, body);
        if !matches!(body.kind, ExprKind::Call(..) | ExprKind::MethodCall(..)) {
            // `|v| v` — the param itself, when it's string-shaped.
            let inner = strip_wraps(cx, typeck, body);
            if is_string_ty(cx, typeck.expr_ty(inner))
                && let Some(hir_id) = param_hir_id(inner, &params.all)
            {
                return if params.source_of.contains_key(&hir_id) {
                    Mapped::Direct(inner)
                } else {
                    Mapped::Projected(inner)
                };
            }
            return Mapped::Silent;
        }
        // `format!(..)` — the AST template is only reachable through the
        // early-pass storage.
        if let Some(macro_call) = root_macro_call_first_node(cx, body)
            && cx.tcx.get_diagnostic_name(macro_call.def_id) == Some(sym::format_macro)
            && let Some(args) = self.format_args.get(cx, body, macro_call.expn)
        {
            return Mapped::Format(args, body);
        }
        // `v.to_string()`, `ToString::to_string(v)`, `v.clone()` over a
        // string.
        let is_string_conversion = is_call_to(cx, typeck, body, &[TO_STRING])
            || (is_call_to(cx, typeck, body, &[CLONE]) && is_string_ty(cx, typeck.expr_ty(body)));
        if !is_string_conversion {
            return Mapped::Silent;
        }
        let Some(receiver) = call_args(body).first().copied() else {
            return Mapped::Silent;
        };
        let receiver = strip_wraps(cx, typeck, receiver);
        match param_hir_id(receiver, &params.all) {
            Some(hir_id) if params.source_of.contains_key(&hir_id) => Mapped::Direct(receiver),
            Some(_) => Mapped::Projected(receiver),
            None if touches_param(receiver, &params.all) => Mapped::Projected(receiver),
            None => Mapped::Silent,
        }
    }

    /// Classify one `format!` argument expression.
    fn classify_arg<'hir>(
        &self,
        cx: &LateContext<'hir>,
        typeck: &TypeckResults<'hir>,
        params: &Params,
        expr: &'hir Expr<'hir>,
    ) -> ArgClass {
        let expr = strip_wraps(cx, typeck, expr);
        if let Some(hir_id) = param_hir_id(expr, &params.all) {
            return if let Some(&index) = params.source_of.get(&hir_id) {
                ArgClass::Param(index)
            } else {
                ArgClass::Projected
            };
        }
        // `x.get()` reads a signal — a `text!` placeholder names the signal
        // itself. Anything else is examined as written.
        let candidate = if is_snapshot_get_in(cx, typeck, expr) {
            get_receiver(expr).map(|receiver| strip_wraps(cx, typeck, receiver))
        } else {
            Some(expr)
        };
        if let Some(captured) = candidate
            && param_hir_id(captured, &params.all).is_none()
            && let Some(name) = bare_path_ident(captured)
            && self.is_signal(cx, typeck, captured)
        {
            return ArgClass::Capture(name);
        }
        if touches_param(expr, &params.all) {
            ArgClass::Projected
        } else if self.is_signal(cx, typeck, expr) {
            ArgClass::ForeignSignal
        } else {
            ArgClass::ForeignValue
        }
    }

    /// `help` for an argument a `text!` slot can't name directly — the user
    /// writes the alias binding by hand. `name` is the chosen slot name.
    fn alias_help(
        &self,
        cx: &LateContext<'_>,
        receiver: &Expr<'_>,
        params: &Params,
        arg: &Expr<'_>,
        name: &str,
        class: ArgClass,
    ) -> String {
        let alias = match class {
            // `receiver.map(|pat| value)` keeps the projection a signal.
            ArgClass::Projected => {
                let receiver_src = snippet_opt(cx, receiver.span);
                let pat = snippet_opt(cx, params.span);
                let value = snippet_opt(cx, arg.span);
                match (receiver_src, pat, value) {
                    (Some(receiver_src), Some(pat), Some(value)) => {
                        format!("{receiver_src}.map(|{pat}| {value})")
                    }
                    _ => name.to_owned(),
                }
            }
            // `text!` slots must be signals — a plain value is bound through
            // `constant(..)`; a non-bare signal aliases as written.
            ArgClass::ForeignValue => match snippet_opt(cx, arg.span) {
                Some(src) => format!("constant({src})"),
                None => name.to_owned(),
            },
            _ => snippet_opt(cx, arg.span).unwrap_or_else(|| name.to_owned()),
        };
        format!(
            "`text!` placeholders take identifiers bound to signals — bind the value under a name: \
             `text!(\"{{{name}}}\", {name} = {alias})`"
        )
    }

    /// Rebuild the `text!` literal (and bindings) the flagged chain should
    /// have been. `typing.call` types the outer body (the map receiver);
    /// `typing.closure` types the closure body the format arguments live in.
    fn render<'a, 'hir>(
        &'a self,
        cx: &LateContext<'hir>,
        typing: Typing<'hir>,
        receiver: &'hir Expr<'hir>,
        params: &Params,
        mapped: &Mapped<'a, 'hir>,
    ) -> Render {
        let mut namer = Namer::new(sources(cx, typing.call, receiver), params);
        match mapped {
            Mapped::Silent => Render::Silent,
            Mapped::Projected(arg) => Render::Help(self.alias_help(
                cx,
                receiver,
                params,
                arg,
                &alias_name(arg, 0),
                ArgClass::Projected,
            )),
            Mapped::Direct(receiver) => {
                // `|v| v.to_string()` — the receiver's source index.
                let Some(hir_id) = param_hir_id(receiver, &params.all) else {
                    return Render::Silent;
                };
                let Some(&index) = params.source_of.get(&hir_id) else {
                    return Render::Silent;
                };
                let Some(name) = namer.slot(cx, index).map(str::to_owned) else {
                    return Render::Silent;
                };
                Render::Text(Rendered {
                    template: format!("{{{name}}}"),
                    bindings: namer.bindings,
                    all_bare: namer.all_bare,
                })
            }
            Mapped::Format(args, call) => {
                let all_args = args.arguments.all_args();
                // First classify every argument: a format that never touches
                // the signal is not this lint; one that touches it through a
                // non-bare shape downgrades to help.
                let mut classes = Vec::with_capacity(all_args.len());
                let mut has_param = false;
                let mut bad = None;
                for (index, argument) in all_args.iter().enumerate() {
                    let Some(hir_arg) = find_format_arg_expr(call, argument) else {
                        return Render::Silent;
                    };
                    let class = self.classify_arg(cx, typing.closure, params, hir_arg);
                    match class {
                        ArgClass::Param(_) | ArgClass::Projected => has_param = true,
                        _ => {}
                    }
                    if bad.is_none() && !matches!(class, ArgClass::Param(_) | ArgClass::Capture(_))
                    {
                        bad = Some((index, hir_arg, class));
                    }
                    classes.push(class);
                }
                if !has_param {
                    return Render::Silent;
                }
                if let Some((index, arg, class)) = bad {
                    let name = all_args[index]
                        .kind
                        .ident()
                        .map(|ident| ident.name.to_string())
                        .unwrap_or_else(|| alias_name(arg, index));
                    return Render::Help(self.alias_help(cx, receiver, params, arg, &name, class));
                }
                // Every argument is a bare source param or a captured name —
                // resolve slot names, then emit the literal verbatim.
                let mut names: Vec<String> = Vec::with_capacity(all_args.len());
                for class in &classes {
                    let name = match class {
                        ArgClass::Param(index) => namer.slot(cx, *index).map(str::to_owned),
                        ArgClass::Capture(name) => Some(name.to_string()),
                        _ => None,
                    };
                    let Some(name) = name else {
                        return Render::Silent;
                    };
                    names.push(name);
                }
                let mut template = String::new();
                for piece in &args.template {
                    match piece {
                        FormatArgsPiece::Literal(text) => {
                            escape_literal(text.as_str(), &mut template)
                        }
                        FormatArgsPiece::Placeholder(placeholder) => {
                            let Ok(index) = placeholder.argument.index else {
                                return Render::Silent;
                            };
                            let Some(spec) = render_options(
                                &placeholder.format_options,
                                placeholder.format_trait,
                            ) else {
                                return Render::Help(format!(
                                    "`text!` can't carry `{{:{}}}`-style dynamic width/precision — \
                                     bind the rendered value under a name instead",
                                    if matches!(
                                        placeholder.format_options.width,
                                        Some(FormatCount::Argument(_))
                                    ) || matches!(
                                        placeholder.format_options.precision,
                                        Some(FormatCount::Argument(_))
                                    ) {
                                        "*"
                                    } else {
                                        "?"
                                    }
                                ));
                            };
                            let Some(name) = names.get(index) else {
                                return Render::Silent;
                            };
                            template.push('{');
                            template.push_str(name);
                            template.push_str(&spec);
                            template.push('}');
                        }
                    }
                }
                Render::Text(Rendered {
                    template,
                    bindings: namer.bindings,
                    all_bare: namer.all_bare,
                })
            }
        }
    }

    /// Merge a rebuild into the existing `text!(..)` at `call_site` —
    /// `text!("{x}", x = <map>)` + template `n = {v:?}` becomes
    /// `text!("n = {v:?}", v = <source>)`.
    fn merge(
        &self,
        cx: &LateContext<'_>,
        call_site: Span,
        slot: Option<Symbol>,
        rendered: &Rendered,
    ) -> Option<String> {
        let src = snippet_opt(cx, call_site)?;
        let parsed = parse_text_call(&src)?;
        // The map's binding must be the call's only `name = expr` — anything
        // else (other aliases) is left alone.
        let [binding] = parsed.bindings.as_slice() else {
            return None;
        };
        if Some(Symbol::intern(&binding.0)) != slot {
            return None;
        }
        // Splice the rendered template in place of the `{slot}` placeholder —
        // only a bare `{slot}` (no spec) can absorb the rewrite.
        let mut template = String::new();
        for piece in text_pieces(&parsed.literal) {
            match piece {
                Piece::Literal(text) => escape_literal(&text, &mut template),
                Piece::Placeholder { name, spec } => {
                    if name != binding.0 {
                        template.push('{');
                        template.push_str(&name);
                        template.push_str(&spec);
                        template.push('}');
                    } else if spec.is_empty() {
                        template.push_str(&rendered.template);
                    } else {
                        return None;
                    }
                }
            }
        }
        let mut out = format!("text!({template:?}");
        if let Some(context) = &parsed.context {
            out.push(' ');
            out.push_str(context);
        }
        for binding in &rendered.bindings {
            out.push_str(", ");
            out.push_str(binding);
        }
        out.push(')');
        Some(out)
    }

    /// `bare_status`-aware suggestion parts: the replacement plus a
    /// `use waterui::text;` when `text` isn't already the macro.
    fn fix_parts(
        &self,
        cx: &LateContext<'_>,
        hir_id: HirId,
        span: Span,
        replacement: &str,
    ) -> Option<Vec<(Span, String)>> {
        let text = self.text_macro(cx)?;
        match bare_status(
            cx,
            hir_id,
            span.lo(),
            Symbol::intern("text"),
            Namespace::MacroNS,
            Some(text),
        ) {
            Bare::Same => Some(vec![(span, replacement.to_owned())]),
            Bare::Free => {
                let (point, before, after) = use_insertion(cx, hir_id, TEXT_USE)?;
                Some(vec![
                    (point, format!("{before}use {TEXT_USE};{after}")),
                    (span, replacement.to_owned()),
                ])
            }
            // `text` names something else here — qualify instead of
            // importing.
            Bare::Conflict | Bare::Unknown => Some(vec![(
                span,
                replacement.replacen("text!", "waterui::text!", 1),
            )]),
        }
    }

    /// The use-position of a `let`-bound map that reaches text.
    fn let_position<'hir>(
        &self,
        cx: &LateContext<'hir>,
        local: &'hir LetStmt<'hir>,
    ) -> Position<'hir> {
        let PatKind::Binding(_, pat_hir, ident, _) = local.pat.kind else {
            return Position::Silent;
        };
        let Some(block) = cx
            .tcx
            .hir_parent_iter(local.hir_id)
            .find_map(|(_, node)| match node {
                Node::Block(block) => Some(block),
                _ => None,
            })
        else {
            return Position::Silent;
        };
        let mut finder = LocalUses {
            pat: pat_hir,
            uses: Vec::new(),
        };
        finder.visit_block(block);
        let mut seen = FxHashSet::default();
        for hir_id in finder.uses {
            if !seen.insert(hir_id) {
                continue;
            }
            let use_typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(hir_id));
            match flow(
                cx,
                use_typeck,
                cx.tcx.hir_expect_expr(hir_id),
                self.text_macro(cx),
            ) {
                Flow::TextMacro { call_site, slot } => {
                    return Position::LetBound {
                        name: ident.name,
                        text_use: TextUse::Macro { call_site, slot },
                    };
                }
                Flow::Arg { call, arg } => match text_position(cx, use_typeck, call, arg) {
                    TextPos::WholeCall | TextPos::Arg => {
                        return Position::LetBound {
                            name: ident.name,
                            text_use: TextUse::Arg,
                        };
                    }
                    TextPos::None => {}
                },
                _ => {}
            }
        }
        Position::Silent
    }

    /// Where this map's result flows.
    fn position<'hir>(
        &self,
        cx: &LateContext<'hir>,
        typeck: &TypeckResults<'hir>,
        expr: &'hir Expr<'hir>,
    ) -> Position<'hir> {
        match flow(cx, typeck, expr, self.text_macro(cx)) {
            Flow::TextMacro { call_site, slot } => Position::TextMacro { call_site, slot },
            Flow::Arg { call, arg } => match text_position(cx, typeck, call, arg) {
                TextPos::WholeCall => Position::TextCall(call),
                TextPos::Arg => Position::BoundArg(arg),
                TextPos::None => Position::Silent,
            },
            Flow::LetInit(local) => self.let_position(cx, local),
            Flow::End => Position::Silent,
        }
    }

    fn report<'hir>(&self, cx: &LateContext<'hir>, typing: Typing<'hir>, flag: &Flag<'_, 'hir>) {
        let render = self.render(cx, typing, flag.receiver, flag.params, flag.mapped);
        if matches!(render, Render::Silent) {
            return;
        }
        span_lint_and_then(
            cx,
            MANUAL_TEXT_MAP,
            flag.expr.span,
            MESSAGE,
            |diag| match flag.position {
                Position::Silent => {}
                Position::TextCall(call) | Position::BoundArg(call) => match render {
                    Render::Text(rendered) => {
                        let applicability = if rendered.all_bare {
                            Applicability::MachineApplicable
                        } else {
                            Applicability::MaybeIncorrect
                        };
                        match self.fix_parts(cx, flag.expr.hir_id, call.span, &rendered.text_call())
                        {
                            Some(parts) => {
                                diag.multipart_suggestion(
                                    "write it as `text!`",
                                    parts,
                                    applicability,
                                );
                            }
                            None => {
                                diag.help(format!("write it as `{}`", rendered.text_call()));
                            }
                        }
                    }
                    Render::Help(help) => {
                        diag.help(help);
                    }
                    Render::Silent => {}
                },
                Position::TextMacro { call_site, slot } => match render {
                    Render::Text(rendered) => match self.merge(cx, call_site, slot, &rendered) {
                        Some(merged) => {
                            let applicability = if rendered.all_bare {
                                Applicability::MachineApplicable
                            } else {
                                Applicability::MaybeIncorrect
                            };
                            match self.fix_parts(cx, flag.expr.hir_id, call_site, &merged) {
                                Some(parts) => {
                                    diag.multipart_suggestion(
                                        "write it as `text!`",
                                        parts,
                                        applicability,
                                    );
                                }
                                None => {
                                    diag.help(format!("write it as `{merged}`"));
                                }
                            }
                        }
                        None => {
                            diag.help(format!("write the `text!` as `{}`", rendered.text_call()));
                        }
                    },
                    Render::Help(help) => {
                        diag.help(help);
                    }
                    Render::Silent => {}
                },
                Position::LetBound { name, text_use } => match render {
                    Render::Text(rendered) => {
                        let target = match text_use {
                            TextUse::Macro { call_site, slot } => self
                                .merge(cx, call_site, slot, &rendered)
                                .unwrap_or_else(|| rendered.text_call()),
                            TextUse::Arg => rendered.text_call(),
                        };
                        diag.help(format!(
                            "write the text position as `{target}` — `{name}` and the `let` both go away"
                        ));
                        diag.note(format!(
                            "`{name}` only carries a `format!` into a text position"
                        ));
                    }
                    Render::Help(help) => {
                        diag.help(help);
                        diag.note(format!(
                            "`{name}` only carries a `format!` into a text position"
                        ));
                    }
                    Render::Silent => {}
                },
            },
        );
    }
}

/// One piece of a `text!`/`format!` literal — brace-escapes are already
/// folded into `Literal` text.
enum Piece {
    Literal(String),
    /// `{name}`/`{name:spec}` — `name` up to `:`/`}`, `spec` including `:`.
    Placeholder {
        name: String,
        spec: String,
    },
}

/// Split a cooked format literal into literals and `{..}` placeholders.
fn text_pieces(literal: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut chars = literal.chars().peekable();
    let mut text = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '{' => {
                let mut content = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == '}' {
                        closed = true;
                        break;
                    }
                    content.push(ch);
                }
                if !closed {
                    // An unterminated `{` — keep it literal.
                    text.push('{');
                    text.push_str(&content);
                    break;
                }
                let (name, spec) = match content.split_once(':') {
                    Some((name, spec)) => (name, format!(":{spec}")),
                    None => (content.as_str(), String::new()),
                };
                if !text.is_empty() {
                    pieces.push(Piece::Literal(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Placeholder {
                    name: name.trim().to_owned(),
                    spec,
                });
            }
            _ => text.push(ch),
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Literal(text));
    }
    pieces
}

impl<'tcx> LateLintPass<'tcx> for ManualTextMap {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        let Some((receiver, func)) = map_call(cx, typeck, expr) else {
            return;
        };
        let Some((pat, body, body_typeck)) = closure_parts(cx.tcx, func) else {
            return;
        };
        let typing = Typing {
            call: typeck,
            closure: body_typeck,
        };
        let source_count = sources(cx, typing.call, receiver).len();
        let params = Params::new(pat, source_count);
        let mapped = self.classify_body(cx, typing.closure, body, &params);
        if matches!(mapped, Mapped::Silent) {
            return;
        }
        let position = self.position(cx, typeck, expr);
        if matches!(position, Position::Silent) {
            return;
        }
        self.report(
            cx,
            typing,
            &Flag {
                expr,
                receiver,
                params: &params,
                mapped: &mapped,
                position,
            },
        );
    }
}
