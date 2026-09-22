//! `manual_string_signal` — a `map` that builds a `String`/`Str` signal by
//! hand (`format!`, `+`/`push_str` concatenation, `from`/`to_string`
//! conversions) restates `s!`, nami's reactive `format!`.

use clippy_utils::macros::{FormatArgsStorage, find_format_arg_expr, root_macro_call_first_node};
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::res::MaybeResPath;
use clippy_utils::source::snippet_opt;
use clippy_utils::ty::implements_trait;
use clippy_utils::visitors::{Descend, for_each_expr, for_each_expr_without_closures};
use rustc_ast::LitKind;
use rustc_ast::format::{FormatArgsPiece, FormatArgumentKind};
use rustc_hir::def_id::DefId;
use rustc_hir::{AssignOpKind, BinOpKind, Block, Expr, ExprKind, LetStmt, Node, PatKind};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::ty::{TypeVisitableExt, TypeckResults};
use rustc_session::impl_lint_pass;
use rustc_span::{ExpnKind, MacroKind, Symbol, sym};
use std::cell::OnceCell;
use std::ops::ControlFlow;

use crate::carriers::{
    CLONE, FROM, INTO, TO_OWNED, TO_STRING, is_string_ty, resolves_to, strip_tail, strip_wraps,
};
use crate::diagnostics::span_lint_and_then;
use crate::format_args::{escape_literal, render_options};
use crate::param_bounds::{TEXT_PARAM_BOUNDS, arg_has_bound, call_args};
use crate::signal_map::{
    Params, RESULT_ADAPTERS, alias_name, bare_path_ident, map_call, mapped_fn, param_hir_id,
    sources, touches_param,
};
use crate::snapshot_get::{get_receiver, is_snapshot_get_in};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags a `SignalExt::map`/`nami::map` whose function — a closure
    /// literal or a path naming a local `fn`/method — produces a
    /// `String`/`Str` and builds it by hand: `format!(..)` on any path,
    /// `String::from(..)`/`Str::from(..)` of a formatted or converted
    /// value, `.to_string()` inside a larger composition, or
    /// `+`/`+=`/`push_str` concatenation — each reading a function
    /// parameter. The result must not reach an `IntoText`/`IntoLabel`
    /// parameter: that case is `manual_text_map`'s `text!` rewrite.
    ///
    /// Silent when the body's only work is a plain conversion of the value
    /// (`|v| v.to_string()`, `|v| Str::from(v)` — `map_into` territory) or
    /// when branches produce only literals (`if v { "on" } else { "off" }`
    /// is `select`).
    ///
    /// ### Why is this bad?
    ///
    /// `s!` is the reactive `format!`: `s!("{k}k", k = &count.map(|c| c /
    /// 1000))` subscribes to its placeholders and yields the `String`
    /// signal directly, with `format!`'s `{name}` capture and `name =
    /// &expr` forms. A hand-written map freezes the formatting into a
    /// closure type and splits the decision from the format.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let label = count.map(|c| format!("{c} items"));
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// let label = s!("{count} items");
    /// ```
    pub MANUAL_STRING_SIGNAL,
    style,
    "a `map` that builds a `String`/`Str` by hand restates `s!`"
}

const MESSAGE: &str = "this `map` builds a string signal by hand; `s!` formats signals directly";

/// `String::push_str` — in-place concatenation.
const PUSH_STR: &[&str] = &["alloc", "string", "String", "push_str"];

/// `Computed::new` — the other spelling of `.computed()`, transparent on
/// the way to the map result's consumer.
const COMPUTED_NEW: &[&str] = &[
    "nami",
    "reactive_core",
    "signal",
    "computed",
    "Computed",
    "new",
];

/// Calls that rewrap a produced string without reading it — peeled both
/// when the map's body is classified and when an `s!` slot value is named.
const CONVERSIONS: &[&[&str]] = &[TO_STRING, TO_OWNED, INTO, FROM];

/// What in the body marks the string as built by hand. Every variant
/// carries the offending expression, whose span becomes the diagnostic's
/// secondary label.
enum Trigger<'hir> {
    /// `format!(..)` whose arguments read the parameter.
    Format(&'hir Expr<'hir>),
    /// `.to_string()` on parameter-derived text — counts only inside a
    /// larger composition; alone (`|v| v.to_string()`) it is the
    /// `map_into` case this lint leaves silent.
    ToString(&'hir Expr<'hir>),
    /// `String::from`/`Str::from` of a formatted or converted value.
    From(&'hir Expr<'hir>),
    /// `+`/`+=` producing a string from parameter-derived operands.
    Concat(&'hir Expr<'hir>),
    /// `push_str` appending parameter-derived text.
    PushStr(&'hir Expr<'hir>),
}

/// `expr` is the first HIR node of a `format!(..)` expansion — the only
/// node `root_macro_call_first_node` reports for that expansion.
fn is_format_call(cx: &LateContext<'_>, expr: &Expr<'_>) -> bool {
    root_macro_call_first_node(cx, expr)
        .is_some_and(|call| cx.tcx.get_diagnostic_name(call.def_id) == Some(sym::format_macro))
}

/// `arg` is a formatted or converted value — it contains a `format!(..)`
/// or a `to_string`/`to_owned` conversion — so wrapping it in
/// `String::from`/`Str::from` is string-building, not the `Str::from(v)`
/// plain conversion this lint ignores.
fn converts<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    arg: &'tcx Expr<'tcx>,
) -> bool {
    for_each_expr_without_closures(arg, |e| {
        if is_format_call(cx, e) || resolves_to(cx, typeck, e, &[TO_STRING, TO_OWNED]) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_some()
}

/// Every hand-built-string marker inside `body` (the map function's body
/// value) that reads a binding of `params`, in source order. Nested
/// closures and const blocks are not descended into — their `format!`s
/// belong to their own bodies.
fn triggers<'hir>(
    cx: &LateContext<'hir>,
    typeck: &TypeckResults<'hir>,
    body: &'hir Expr<'hir>,
    params: &Params,
) -> Vec<Trigger<'hir>> {
    let mut out = Vec::new();
    for_each_expr(cx, body, |e| -> ControlFlow<(), Descend> {
        if matches!(e.kind, ExprKind::Closure(_) | ExprKind::ConstBlock(_)) {
            return ControlFlow::Continue(Descend::No);
        }
        if is_format_call(cx, e) && touches_param(e, &params.all) {
            out.push(Trigger::Format(e));
            return ControlFlow::Continue(Descend::Yes);
        }
        match e.kind {
            ExprKind::Binary(op, ..) if op.node == BinOpKind::Add => {
                if is_string_ty(cx, typeck.expr_ty(e)) && touches_param(e, &params.all) {
                    out.push(Trigger::Concat(e));
                }
            }
            ExprKind::AssignOp(op, lhs, _) if op.node == AssignOpKind::AddAssign => {
                // `v += "!"` accumulates onto the parameter itself — test
                // the whole assignment, both operands.
                if is_string_ty(cx, typeck.expr_ty(lhs)) && touches_param(e, &params.all) {
                    out.push(Trigger::Concat(e));
                }
            }
            ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                let args = call_args(e);
                if resolves_to(cx, typeck, e, &[TO_STRING]) {
                    if args
                        .first()
                        .is_some_and(|receiver| touches_param(receiver, &params.all))
                    {
                        out.push(Trigger::ToString(e));
                    }
                } else if resolves_to(cx, typeck, e, &[FROM]) {
                    if is_string_ty(cx, typeck.expr_ty(e))
                        && args.first().is_some_and(|arg| converts(cx, typeck, arg))
                    {
                        out.push(Trigger::From(e));
                    }
                } else if resolves_to(cx, typeck, e, &[PUSH_STR])
                    && args.iter().any(|arg| touches_param(arg, &params.all))
                {
                    out.push(Trigger::PushStr(e));
                }
            }
            _ => {}
        }
        ControlFlow::Continue(Descend::Yes)
    });
    out
}

/// The body decides between string-producing `if`/`match` branches — the
/// help then names the condition-as-signal rewrite (`sig.equal_to(..)`/
/// `sig.ge(..)`) alongside `s!`.
fn has_string_branch<'tcx>(
    cx: &LateContext<'tcx>,
    typeck: &TypeckResults<'tcx>,
    body: &'tcx Expr<'tcx>,
) -> bool {
    for_each_expr(cx, body, |e| -> ControlFlow<(), Descend> {
        if matches!(e.kind, ExprKind::Closure(_) | ExprKind::ConstBlock(_)) {
            return ControlFlow::Continue(Descend::No);
        }
        if matches!(e.kind, ExprKind::If(..) | ExprKind::Match(..))
            && is_string_ty(cx, typeck.expr_ty(e))
        {
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(Descend::Yes)
    })
    .is_some()
}

/// Where the map's result ends up after climbing transparent consumers:
/// an argument position, a `let` initializer, a `text!` capture binding —
/// or something this lint reads as not-text.
enum Sink<'hir> {
    /// The peeled expression is argument `usize` of the call.
    Arg(&'hir Expr<'hir>, usize),
    /// The peeled expression initializes the `let`.
    Let(&'hir LetStmt<'hir>),
    /// The peeled expression is a `text!` capture's value —
    /// `manual_text_map`'s territory, so it counts as reaching text.
    TextCapture,
    /// Consumed any other way — a field, a `.get()`, a return, a drop.
    End,
}

/// Climb `expr`'s parents through drop-temps, statement-free block tails,
/// `if`/`match` arms, and the signal carriers (`computed`/`with`/`cached`/
/// `clone`/`into`/`from`/`Computed::new`) at operand position 0. A call
/// argument reports `Arg`, a `let` initializer `Let` — or `TextCapture`
/// when the `let` is a `text!` capture binding.
fn sink<'hir>(cx: &LateContext<'hir>, expr: &'hir Expr<'hir>, text: Option<DefId>) -> Sink<'hir> {
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
                ExprKind::Call(..) | ExprKind::MethodCall(..) => {
                    let args = call_args(parent);
                    let Some(index) = args.iter().position(|a| a.hir_id == current.hir_id) else {
                        // `current` is the callee, not an operand.
                        return Sink::End;
                    };
                    // The parent may live in a different body than `expr`.
                    let typeck = cx
                        .tcx
                        .typeck(cx.tcx.hir_enclosing_body_owner(parent.hir_id));
                    let carrier = resolves_to(cx, typeck, parent, RESULT_ADAPTERS)
                        || resolves_to(
                            cx,
                            typeck,
                            parent,
                            &[CLONE, TO_OWNED, INTO, FROM, COMPUTED_NEW],
                        );
                    if index == 0 && carrier {
                        current = parent;
                    } else {
                        return Sink::Arg(parent, index);
                    }
                }
                _ => return Sink::End,
            },
            Node::LetStmt(local)
                if local.init.is_some_and(|init| init.hir_id == current.hir_id) =>
            {
                return if is_text_capture(local, text) {
                    Sink::TextCapture
                } else {
                    Sink::Let(local)
                };
            }
            Node::Arm(arm) if arm.body.hir_id == current.hir_id => {}
            _ => return Sink::End,
        }
    }
    Sink::End
}

/// The `let` is one of `text!`'s capture bindings — its span sits in the
/// `text!` expansion (by macro name, or by `macro_def_id` covering
/// `use .. as ..` aliases).
fn is_text_capture(local: &LetStmt<'_>, text: Option<DefId>) -> bool {
    let expn = local.span.ctxt().outer_expn_data();
    match expn.kind {
        ExpnKind::Macro(MacroKind::Bang, name) => {
            name == Symbol::intern("text") || text.is_some_and(|did| expn.macro_def_id == Some(did))
        }
        _ => false,
    }
}

/// Whether `expr`'s value reaches an `IntoText`/`IntoLabel` parameter —
/// through the carriers `sink` climbs, or through a `let` whose binding
/// has at least one use that does.
fn reaches_text<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>, text: Option<DefId>) -> bool {
    match sink(cx, expr, text) {
        Sink::Arg(call, index) => {
            let typeck = cx.tcx.typeck(cx.tcx.hir_enclosing_body_owner(call.hir_id));
            arg_has_bound(cx, typeck, call, &[TEXT_PARAM_BOUNDS], index)
        }
        Sink::TextCapture => true,
        Sink::Let(local) => {
            let PatKind::Binding(_, pat, _, None) = local.pat.kind else {
                return false;
            };
            let body = cx
                .tcx
                .hir_body_owned_by(cx.tcx.hir_enclosing_body_owner(local.hir_id));
            for_each_expr(cx, body.value, |e| {
                if e.res_local_id() == Some(pat) && reaches_text(cx, e, text) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(Descend::Yes)
                }
            })
            .is_some()
        }
        Sink::End => false,
    }
}

/// `expr` stripped of `&`/drop-temps/`clone` and the string conversions —
/// `to_string`/`to_owned`/`into`/`from` — down to the value rendered.
fn peel_value<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    mut expr: &'hir Expr<'hir>,
) -> &'hir Expr<'hir> {
    loop {
        expr = strip_wraps(cx, typeck, expr);
        if matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
            && resolves_to(cx, typeck, expr, CONVERSIONS)
            && let Some(arg) = call_args(expr).first()
        {
            expr = arg;
            continue;
        }
        return expr;
    }
}

/// The one signal source an argument's bindings destructure — `u.name`
/// reads source 0's binding `u`. `None` when it reads no binding, a
/// partial destructure, or bindings of several sources.
fn single_source(expr: &Expr<'_>, params: &Params) -> Option<usize> {
    let mut found = None;
    for_each_expr_without_closures(expr, |e| {
        if let Some(hir_id) = param_hir_id(e, &params.all) {
            match (found, params.source_of.get(&hir_id)) {
                (None, Some(&i)) => found = Some(i),
                (Some(j), Some(&i)) if i == j => {}
                _ => return ControlFlow::Break(()),
            }
        }
        ControlFlow::Continue(())
    })
    .is_none()
    .then_some(found)
    .flatten()
}

/// How an `s!` placeholder gets its value.
enum Slot {
    /// `{name}` captures the in-scope signal directly — no binding.
    Capture(String),
    /// `{name}` plus a `name = <signal>` binding after the literal.
    Bind(String, String),
}

/// The `name = ..` arguments an `s!` needs for `slots` — `(placeholder
/// name, bound value)` pairs where `None` marks a capture. One bound
/// argument puts `s!` in named mode, which rejects a `{name}` placeholder
/// with no `name = ..` argument — so captures then bind too. All-capture
/// inputs emit no arguments at all. nami-derive 0.3.1 feeds each named
/// value through `ToOwned::to_owned(expr)` — a UFCS call that does not
/// auto-ref — so every value is spelled `&expr`.
fn s_args(slots: &[(String, Option<String>)]) -> Vec<String> {
    if slots.iter().all(|(_, rhs)| rhs.is_none()) {
        return Vec::new();
    }
    slots
        .iter()
        .map(|(name, rhs)| match rhs {
            Some(rhs) => format!("{name} = &{rhs}"),
            None => format!("{name} = &{name}"),
        })
        .collect()
}

pub(crate) struct ManualStringSignal {
    format_args: FormatArgsStorage,
    signal: OnceCell<Option<DefId>>,
    text_macro: OnceCell<Option<DefId>>,
}

impl_lint_pass!(ManualStringSignal => [MANUAL_STRING_SIGNAL]);

impl ManualStringSignal {
    pub(crate) fn new(format_args: FormatArgsStorage) -> Self {
        Self {
            format_args,
            signal: OnceCell::new(),
            text_macro: OnceCell::new(),
        }
    }

    /// `nami_core::Signal`'s `DefId` — for `is_signal` classification.
    fn signal(&self, cx: &LateContext<'_>) -> Option<DefId> {
        *self.signal.get_or_init(|| {
            lookup_path_str(cx.tcx, PathNS::Type, "nami_core::Signal")
                .first()
                .copied()
        })
    }

    /// `waterui_macros::text`'s `DefId` — the `text!` expansion mark that
    /// makes a `let` a capture binding.
    fn text_macro(&self, cx: &LateContext<'_>) -> Option<DefId> {
        *self.text_macro.get_or_init(|| {
            lookup_path_str(cx.tcx, PathNS::Macro, "waterui_macros::text")
                .first()
                .copied()
        })
    }

    /// Whether `expr` is a `Signal`-typed value.
    fn is_signal_ty<'tcx>(
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

    /// Whether `expr` is `Clone`-able — `s!` binds captured signals via
    /// `ident.clone()` and named arguments via `ToOwned::to_owned(..)`,
    /// so a value it cannot copy cannot fill a slot.
    fn is_clone<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        typeck: &TypeckResults<'tcx>,
        expr: &Expr<'tcx>,
    ) -> bool {
        let Some(clone) = cx.tcx.lang_items().clone_trait() else {
            return false;
        };
        let ty = typeck.expr_ty(expr);
        !ty.has_infer() && implements_trait(cx, ty, clone, &[])
    }

    /// Whether `expr` is a `Clone`-able `Signal` — a value `s!` may
    /// capture or bind. A non-`Clone` signal withholds the sketch
    /// entirely (the emitted code could not compile).
    fn is_signal<'tcx>(
        &self,
        cx: &LateContext<'tcx>,
        typeck: &TypeckResults<'tcx>,
        expr: &Expr<'tcx>,
    ) -> bool {
        self.is_signal_ty(cx, typeck, expr) && self.is_clone(cx, typeck, expr)
    }

    /// The `s!` slot an argument expression fills: a parameter binding maps
    /// back to its signal source (captured by the source's own name when
    /// that is a bare identifier), `x.get()` captures `x`, a bare in-scope
    /// signal captures by name, a parameter projection becomes
    /// `name = <source>.map(|<binding>| <expr>)`, and any other value is
    /// bound through `constant(..)`.
    fn slot<'hir>(
        &self,
        cx: &LateContext<'hir>,
        typeck: &TypeckResults<'hir>,
        sources: &[&'hir Expr<'hir>],
        params: &Params,
        expr: &'hir Expr<'hir>,
        index: usize,
    ) -> Option<Slot> {
        let expr = peel_value(cx, typeck, expr);
        if let Some(hir_id) = param_hir_id(expr, &params.all)
            && let Some(&i) = params.source_of.get(&hir_id)
        {
            // `sources` live in the map call's body — `typeck` is the map
            // function's — so the source's own body types it.
            let source = sources[i];
            let source_typeck = cx
                .tcx
                .typeck(cx.tcx.hir_enclosing_body_owner(source.hir_id));
            // The source is read through `clone`/`to_owned` — a
            // non-`Clone` signal cannot fill the slot.
            if !self.is_clone(cx, source_typeck, source) {
                return None;
            }
            if let Some(name) = bare_path_ident(source) {
                return Some(Slot::Capture(name.to_string()));
            }
            let name = params.names[i]
                .map(|name| name.to_string())
                .unwrap_or_else(|| format!("arg{i}"));
            return Some(Slot::Bind(name, snippet_opt(cx, source.span)?));
        }
        if is_snapshot_get_in(cx, typeck, expr)
            && let Some(receiver) = get_receiver(expr)
                .map(|receiver| strip_wraps(cx, typeck, receiver))
                .filter(|receiver| self.is_clone(cx, typeck, receiver))
            && let Some(name) = bare_path_ident(receiver)
        {
            return Some(Slot::Capture(name.to_string()));
        }
        if let Some(name) = bare_path_ident(expr) {
            // `s!` captures the signal as `name.clone()` — a `Signal`
            // that is not `Clone` cannot fill the slot, so withhold the
            // sketch.
            if self.is_signal_ty(cx, typeck, expr) {
                return self
                    .is_signal(cx, typeck, expr)
                    .then(|| Slot::Capture(name.to_string()));
            }
            // `constant(name)` still goes through `to_owned`, needing
            // `Clone`.
            return self
                .is_clone(cx, typeck, expr)
                .then(|| Slot::Bind(name.to_string(), format!("constant({name})")));
        }
        if touches_param(expr, &params.all) {
            let i = single_source(expr, params)?;
            let pat = params.names[i]?.to_string();
            return Some(Slot::Bind(
                alias_name(expr, index),
                format!(
                    "{}.map(|{pat}| {})",
                    snippet_opt(cx, sources[i].span)?,
                    snippet_opt(cx, expr.span)?
                ),
            ));
        }
        let source = snippet_opt(cx, expr.span)?;
        if self.is_signal_ty(cx, typeck, expr) {
            return self
                .is_signal(cx, typeck, expr)
                .then(|| Slot::Bind(format!("arg{index}"), source));
        }
        self.is_clone(cx, typeck, expr)
            .then(|| Slot::Bind(format!("arg{index}"), format!("constant({source})")))
    }

    /// `s!("..", ..)` for a body that is a single `format!(..)` (after the
    /// `from`/`into` tail wraps peel). `None` when the template or an
    /// argument cannot be mapped back to source — the diagnostic then
    /// carries the generic `s!` help.
    fn format_sketch<'hir>(
        &self,
        cx: &LateContext<'hir>,
        typeck: &TypeckResults<'hir>,
        sources: &[&'hir Expr<'hir>],
        params: &Params,
        body: &'hir Expr<'hir>,
    ) -> Option<String> {
        let macro_call = root_macro_call_first_node(cx, body)?;
        let format_args = self.format_args.get(cx, body, macro_call.expn)?;
        let mut slots = Vec::new();
        for (index, argument) in format_args.arguments.all_args().iter().enumerate() {
            let hir_arg = find_format_arg_expr(body, argument)?;
            if let FormatArgumentKind::Named(ident) = argument.kind {
                let rhs = match self.slot(cx, typeck, sources, params, hir_arg, index)? {
                    Slot::Capture(name) | Slot::Bind(_, name) => name,
                };
                slots.push((ident.to_string(), Some(rhs)));
            } else {
                match self.slot(cx, typeck, sources, params, hir_arg, index)? {
                    Slot::Capture(name) => slots.push((name, None)),
                    Slot::Bind(name, rhs) => slots.push((name, Some(rhs))),
                }
            }
        }
        let mut template = String::new();
        for piece in &format_args.template {
            match piece {
                FormatArgsPiece::Literal(text) => escape_literal(text.as_str(), &mut template),
                FormatArgsPiece::Placeholder(placeholder) => {
                    let index = placeholder.argument.index.ok()?;
                    let spec =
                        render_options(&placeholder.format_options, placeholder.format_trait)?;
                    let (name, _) = slots.get(index)?;
                    template.push('{');
                    template.push_str(name);
                    template.push_str(&spec);
                    template.push('}');
                }
            }
        }
        let mut sketch = format!("s!({template:?}");
        for arg in s_args(&slots) {
            sketch.push_str(", ");
            sketch.push_str(&arg);
        }
        sketch.push(')');
        Some(sketch)
    }

    /// `s!("..", ..)` for a body that is a `+` chain — each operand is a
    /// string literal (folded into the template) or a slot.
    fn concat_sketch<'hir>(
        &self,
        cx: &LateContext<'hir>,
        typeck: &TypeckResults<'hir>,
        sources: &[&'hir Expr<'hir>],
        params: &Params,
        body: &'hir Expr<'hir>,
    ) -> Option<String> {
        fn leaves<'hir>(expr: &'hir Expr<'hir>, out: &mut Vec<&'hir Expr<'hir>>) {
            match expr.kind {
                ExprKind::Binary(op, lhs, rhs) if op.node == BinOpKind::Add => {
                    leaves(lhs, out);
                    leaves(rhs, out);
                }
                _ => out.push(expr),
            }
        }
        let mut operands = Vec::new();
        leaves(body, &mut operands);
        let mut slots = Vec::new();
        let mut template = String::new();
        for (index, operand) in operands.into_iter().enumerate() {
            let operand = peel_value(cx, typeck, operand);
            if let ExprKind::Lit(lit) = operand.kind
                && let LitKind::Str(text, _) = lit.node
            {
                escape_literal(text.as_str(), &mut template);
                continue;
            }
            let (name, rhs) = match self.slot(cx, typeck, sources, params, operand, index)? {
                Slot::Capture(name) => (name, None),
                Slot::Bind(name, rhs) => (name, Some(rhs)),
            };
            slots.push((name.clone(), rhs));
            template.push('{');
            template.push_str(&name);
            template.push('}');
        }
        let mut sketch = format!("s!({template:?}");
        for arg in s_args(&slots) {
            sketch.push_str(", ");
            sketch.push_str(&arg);
        }
        sketch.push(')');
        Some(sketch)
    }

    /// The `s!("..")` help the flagged body maps to — a `format!` body or a
    /// `+` chain renders concretely; anything else returns `None` and the
    /// diagnostic shows the generic spelling.
    fn s_sketch<'hir>(
        &self,
        cx: &LateContext<'hir>,
        typeck: &TypeckResults<'hir>,
        sources: &[&'hir Expr<'hir>],
        params: &Params,
        body: &'hir Expr<'hir>,
    ) -> Option<String> {
        let body = strip_tail(cx, typeck, body);
        if is_format_call(cx, body) {
            return self.format_sketch(cx, typeck, sources, params, body);
        }
        if matches!(body.kind, ExprKind::Binary(op, ..) if op.node == BinOpKind::Add)
            && is_string_ty(cx, typeck.expr_ty(body))
        {
            return self.concat_sketch(cx, typeck, sources, params, body);
        }
        None
    }
}

impl<'tcx> LateLintPass<'tcx> for ManualStringSignal {
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let typeck = cx.typeck_results();
        let Some((receiver, func)) = map_call(cx, typeck, expr) else {
            return;
        };
        let Some((pat, value, body_typeck)) = mapped_fn(cx, typeck, func) else {
            return;
        };
        if !is_string_ty(cx, body_typeck.expr_ty(value)) {
            return;
        }
        let source_list = sources(cx, typeck, receiver);
        let params = Params::new(pat, source_list.len());
        if params.all.is_empty() {
            return;
        }
        let found = triggers(cx, body_typeck, value, &params);
        let Some(first) = found.first() else {
            return;
        };
        // `|v| v.to_string()` alone — under `.into()`/`Str::from(..)`
        // wrappers too — is a plain conversion, `map_into` territory,
        // not a hand-built string. Judge the body after `strip_tail`.
        let stripped = strip_tail(cx, body_typeck, value);
        if let [Trigger::ToString(call)] = triggers(cx, body_typeck, stripped, &params).as_slice()
            && stripped.hir_id == call.hir_id
            && call_args(call).first().is_some_and(|r| {
                param_hir_id(strip_wraps(cx, body_typeck, r), &params.all).is_some()
            })
        {
            return;
        }
        // A `String`/`Str` signal that reaches an `IntoText`/`IntoLabel`
        // parameter is `manual_text_map`'s `text!` case, not this lint's.
        if reaches_text(cx, expr, self.text_macro(cx)) {
            return;
        }
        let label_span = match first {
            Trigger::Format(e)
            | Trigger::ToString(e)
            | Trigger::From(e)
            | Trigger::Concat(e)
            | Trigger::PushStr(e) => e.span,
        };
        let sketch = self.s_sketch(cx, body_typeck, &source_list, &params, value);
        let branchy = has_string_branch(cx, body_typeck, value);
        let receiver_src = snippet_opt(cx, receiver.span).unwrap_or_else(|| "count".into());
        span_lint_and_then(cx, MANUAL_STRING_SIGNAL, expr.span, MESSAGE, |diag| {
            diag.span_label(label_span, "the string is built by hand here");
            match sketch {
                Some(sketch) => {
                    diag.help(format!(
                        "use `{sketch}` — `s!` subscribes to its placeholders the way \
                         `format!` captures names"
                    ));
                }
                None => {
                    diag.help(format!(
                        "use `s!` — `s!(\"{{k}}k\", k = &{receiver_src}.map(|c| c / 1000))`"
                    ));
                }
            }
            if branchy {
                diag.help(format!(
                    "a branch condition becomes a signal too — \
                     `{receiver_src}.equal_to(0).select(..)` / `{receiver_src}.ge(10_000)` — \
                     so the decision is a signal instead of a re-run closure"
                ));
            }
        });
    }
}
