//! `manual_identifiable` — a hand-written `impl Identifiable` or a
//! `use_id`/`self_id` wrapper on a local struct where `#[derive(Identifiable)]`
//! plus a `#[id]` field marker say the same thing.

use clippy_utils::is_self;
use clippy_utils::paths::{PathNS, lookup_path_str};
use clippy_utils::source::{snippet_indent, snippet_opt};
use clippy_utils::ty::implements_trait;
use std::sync::Arc;

use rustc_errors::Applicability;
use rustc_hir::attrs::AttributeKind;
use rustc_hir::def::{Namespace, Res};
use rustc_hir::def_id::DefId;
use rustc_hir::{
    Attribute, Body, Expr, ExprKind, FieldDef, HirId, ImplItemImplKind, ImplItemKind, Item,
    ItemKind, Node, Pat, PatKind, QPath,
};
use rustc_lint::{LateContext, LateLintPass, LintContext};
use rustc_middle::ty::{
    ConstKind, GenericArgKind, GenericArgsRef, RegionKind, Ty, TyKind, TypeckResults,
};
use rustc_session::declare_lint_pass;
use rustc_span::source_map::SourceMap;
use rustc_span::symbol::Symbol;
use rustc_span::{BytePos, Pos, Span};

use crate::def_path::def_path_eq;
use crate::diagnostics::span_lint_and_then;
use crate::imports::{Bare, bare_status};
use crate::param_bounds::{call_args, call_def_id, implemented_trait_item};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags two hand-rolled spellings of `Identifiable` on structs defined in
    /// the current crate:
    ///
    /// * `impl Identifiable for T` whose `id` simply returns one field
    ///   (`self.f`, `self.f.clone()`, `self.0`) and whose `type Id` is that
    ///   field's type.
    /// * `x.use_id(|v| v.f)`/`x.self_id()` — the `IdentifiableExt` wrappers —
    ///   on a value whose type is a local struct.
    ///
    /// `#[derive(Identifiable)]` with `#[id]` on the field covers both.
    ///
    /// ### Why is this bad?
    ///
    /// `Identifiable` is a marking trait: the derive declares the identity
    /// next to the data, once. A manual impl splits that declaration across a
    /// second item, and `use_id`/`self_id` exist to wrap *foreign* types —
    /// reaching for them on a local type keeps a wrapper (`UseId`/`SelfId`)
    /// the struct never needed.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// struct Node { id: u64, data: Data }
    /// impl Identifiable for Node {
    ///     type Id = u64;
    ///     fn id(&self) -> Self::Id { self.id }
    /// }
    /// ```
    ///
    /// Use instead:
    ///
    /// ```rust,ignore
    /// #[derive(Identifiable)]
    /// struct Node { #[id] id: u64, data: Data }
    /// ```
    ///
    /// ### Known problems
    ///
    /// The fix is `MaybeIncorrect`: `#[derive(Identifiable)]` expands to
    /// `impl Identifiable`, so the trait must be reachable as `Identifiable`
    /// in the struct's module. `use waterui::Identifiable` provides it;
    /// a file that only imports `IdentifiableExt` needs the trait imported by
    /// hand after the fix.
    pub MANUAL_IDENTIFIABLE,
    style,
    "hand-written `impl Identifiable` or `use_id`/`self_id` where the derive fits"
}

declare_lint_pass!(ManualIdentifiable => [MANUAL_IDENTIFIABLE]);

/// `waterui_core::foundation::id::Identifiable` — the trait. Users reach it
/// through `use waterui::Identifiable` or `waterui::id::Identifiable`.
const IDENTIFIABLE: &[&str] = &["waterui_core", "foundation", "id", "Identifiable"];

/// `IdentifiableExt::{use_id, self_id}` — the wrappers that exist to give
/// foreign types an identity.
const USE_ID: &[&str] = &[
    "waterui_core",
    "foundation",
    "id",
    "IdentifiableExt",
    "use_id",
];
const SELF_ID: &[&str] = &[
    "waterui_core",
    "foundation",
    "id",
    "IdentifiableExt",
    "self_id",
];

/// `Clone::clone` — the only call allowed around `self.f`/`v.f`.
const CLONE: &[&str] = &["core", "clone", "Clone", "clone"];

/// What the closure of a `use_id` selects.
enum Mark {
    /// `|v| v.f` / `|v| v.0` — the `#[id]` field by name.
    Field(Symbol),
    /// `self_id()` / `use_id(|v| v.clone())` — the struct's only field.
    Whole,
}

/// The HIR item of a local struct type's definition, `None` for enums,
/// unions, foreign types, and macro-generated items.
fn local_struct_item<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> Option<&'tcx Item<'tcx>> {
    let TyKind::Adt(adt, _) = ty.kind() else {
        return None;
    };
    if !adt.is_struct() || !adt.did().is_local() {
        return None;
    }
    let Node::Item(item) = cx
        .tcx
        .hir_node(cx.tcx.local_def_id_to_hir_id(adt.did().expect_local()))
    else {
        return None;
    };
    if !matches!(item.kind, ItemKind::Struct(..)) || item.span.from_expansion() {
        return None;
    }
    Some(item)
}

/// The `FieldDef` named `name` — `"0"`/`"1"`/`..` name tuple fields.
fn field_by_name<'hir>(item: &'hir Item<'hir>, name: Symbol) -> Option<&'hir FieldDef<'hir>> {
    let ItemKind::Struct(.., vdata) = &item.kind else {
        return None;
    };
    vdata.fields().iter().find(|field| field.ident.name == name)
}

/// The only field of a single-field struct — `self_id`/`v.clone()` can only
/// become `#[id]` there.
fn single_field<'hir>(item: &'hir Item<'hir>) -> Option<&'hir FieldDef<'hir>> {
    let ItemKind::Struct(.., vdata) = &item.kind else {
        return None;
    };
    match vdata.fields() {
        [field] => Some(field),
        _ => None,
    }
}

/// `expr` is a path to the `hir_id` binding — `self` or the closure's `v`.
fn is_local(expr: &Expr<'_>, hir_id: HirId) -> bool {
    matches!(
        expr.kind,
        ExprKind::Path(QPath::Resolved(None, path)) if path.res == Res::Local(hir_id)
    )
}

/// The `hir_id` a `|v|`/`self` pattern binds.
fn binding_hir(pat: &Pat<'_>) -> Option<HirId> {
    if let PatKind::Binding(_, hir_id, _, _) = pat.kind {
        Some(hir_id)
    } else {
        None
    }
}

/// `expr` with `{ .. }` blocks and drop-temps peeled — the value a body
/// computes.
fn tail<'hir>(mut expr: &'hir Expr<'hir>) -> &'hir Expr<'hir> {
    loop {
        expr = match expr.kind {
            ExprKind::Block(block, _) if block.stmts.is_empty() => match block.expr {
                Some(inner) => inner,
                None => return expr,
            },
            ExprKind::DropTemps(inner) => inner,
            _ => return expr,
        };
    }
}

/// Whether `expr` calls `Clone::clone` — `x.clone()` under `typeck`.
fn is_clone_call<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &Expr<'hir>,
) -> bool {
    let Some(did) = call_def_id(typeck, expr) else {
        return false;
    };
    def_path_eq(cx, implemented_trait_item(cx.tcx, did), CLONE)
}

/// `expr` with a trailing `.clone()` peeled — `self.f.clone()` → `self.f`,
/// `v.clone()` → `v`.
fn peel_clone<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    expr: &'hir Expr<'hir>,
) -> &'hir Expr<'hir> {
    if let ExprKind::MethodCall(_, receiver, [], _) = expr.kind
        && is_clone_call(cx, typeck, expr)
    {
        receiver
    } else {
        expr
    }
}

/// `expr` reads a field of `local` — `v.f`, `v.0` — returns its name.
fn field_read(expr: &Expr<'_>, local: HirId) -> Option<Symbol> {
    if let ExprKind::Field(base, field) = expr.kind
        && is_local(base, local)
    {
        Some(field.name)
    } else {
        None
    }
}

/// The field `fn id(&self)` returns: `self.f`, `self.f.clone()`, `self.0`,
/// `self.0.clone()` — `None` for a computed body.
fn id_field<'hir>(
    cx: &LateContext<'_>,
    typeck: &TypeckResults<'hir>,
    body: &Body<'hir>,
) -> Option<Symbol> {
    let param = body.params.first()?;
    if !is_self(param) {
        return None;
    }
    let hir_id = binding_hir(param.pat)?;
    field_read(peel_clone(cx, typeck, tail(body.value)), hir_id)
}

/// The `#[id]` target for a `use_id` closure: `|v| v.f`, `|v| v.f.clone()`,
/// `|v| v.0` mark that field; `|v| v.clone()` marks the whole value. `None`
/// for a closure that computes.
fn closure_mark(cx: &LateContext<'_>, func: &Expr<'_>) -> Option<Mark> {
    let mut func = func;
    while let ExprKind::DropTemps(inner) = func.kind {
        func = inner;
    }
    let ExprKind::Closure(closure) = func.kind else {
        return None;
    };
    let body = cx.tcx.hir_body(closure.body);
    let [param] = body.params else {
        return None;
    };
    let hir_id = binding_hir(param.pat)?;
    let typeck = cx.tcx.typeck_body(closure.body);
    let expr = peel_clone(cx, typeck, tail(body.value));
    if is_local(expr, hir_id) {
        Some(Mark::Whole)
    } else {
        field_read(expr, hir_id).map(Mark::Field)
    }
}

/// `args` are the owner generics in order — the impl covers every instance
/// of the struct, exactly what the derive emits. A partial instantiation
/// (`impl<T> Identifiable for TreeNode<T, u64>`) is left alone.
fn identity_args(args: GenericArgsRef<'_>) -> bool {
    args.iter().enumerate().all(|(index, arg)| match arg.kind() {
        GenericArgKind::Type(ty) => {
            matches!(ty.kind(), TyKind::Param(param) if param.index as usize == index)
        }
        GenericArgKind::Lifetime(region) => {
            matches!(region.kind(), RegionKind::ReEarlyParam(param) if param.index as usize == index)
        }
        GenericArgKind::Const(ct) => {
            matches!(ct.kind(), ConstKind::Param(param) if param.index as usize == index)
        }
    })
}

/// The source span an attribute occupies — `#[..]` for unparsed ones, the
/// comment/doc span for parsed ones; `None` for parsed kinds without a
/// retrievable span.
fn attr_span(attr: &Attribute) -> Option<Span> {
    match attr {
        Attribute::Unparsed(item) => Some(item.span),
        Attribute::Parsed(
            AttributeKind::DocComment { span, .. } | AttributeKind::Deprecated { span, .. },
        ) => Some(*span),
        _ => None,
    }
}

/// How the derive is spelled at `item`: bare `Identifiable` when a bare
/// `Identifiable` already resolves to the `waterui_macros` derive there
/// (`use waterui::Identifiable` imports both namespaces), else
/// `waterui::Identifiable`.
fn derive_path(cx: &LateContext<'_>, item: &Item<'_>) -> &'static str {
    let derive = lookup_path_str(cx.tcx, PathNS::Macro, "waterui_macros::Identifiable")
        .first()
        .copied();
    match bare_status(
        cx,
        item.hir_id(),
        item.span.lo(),
        Symbol::intern("Identifiable"),
        Namespace::MacroNS,
        derive,
    ) {
        Bare::Same => "Identifiable",
        _ => "waterui::Identifiable",
    }
}

/// `(insertion point, text)` extending a source `#[derive(..)]` attached to
/// `item`: `, <path>` before the `)` closing the list — a bare `<path>` when
/// the list is empty or already comma-terminated. `None` when no `#[derive]`
/// precedes the item: derive attributes are consumed at expansion and never
/// reach HIR attrs, so the search runs on the source text before the item,
/// skipping back over comments and non-derive attributes.
fn extend_derive_part(sm: &SourceMap, item: &Item<'_>, path: &str) -> Option<(Span, String)> {
    let file = sm.lookup_char_pos(item.span.lo()).file;
    let src = file.src.as_ref()?;
    let mut rest = &src[..(item.span.lo() - file.start_pos).to_usize()];
    loop {
        rest = rest.trim_end();
        // Comments can sit between an attribute and the item.
        let line_start = rest.rfind('\n').map_or(0, |index| index + 1);
        if rest[line_start..].trim_start().starts_with("//") {
            rest = &rest[..line_start];
            continue;
        }
        if rest.ends_with("*/") {
            rest = &rest[..rest[..rest.len() - 2].rfind("/*")?];
            continue;
        }
        if !rest.ends_with(']') {
            return None;
        }
        // Walk `]` back to its `#[`.
        let bytes = rest.as_bytes();
        let mut depth = 0usize;
        let mut open = None;
        for (index, &byte) in bytes.iter().enumerate().rev() {
            match byte {
                b']' => depth += 1,
                b'[' => {
                    depth -= 1;
                    if depth == 0 {
                        open = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let open = open.filter(|&index| index > 0 && bytes[index - 1] == b'#')?;
        // `rest` ends at `]`; the attribute body ends at the last
        // non-whitespace char before it — `)` for `derive(..)`.
        let before = rest[..rest.len() - 1].trim_end();
        let inner = before[open + 1..].trim_start();
        let next = &rest[..open - 1];
        let Some(args) = inner.strip_prefix("derive") else {
            rest = next;
            continue;
        };
        let args = args.trim_start();
        if !args.starts_with('(') || !args.ends_with(')') {
            // `#[derive]` or `#[derive = ..]` — not a path list.
            rest = next;
            continue;
        }
        let close = file.start_pos + BytePos(before.len() as u32 - 1);
        let list = args[1..args.len() - 1].trim_end();
        let text = if list.is_empty() {
            path.to_string()
        } else if list.ends_with(',') {
            format!(" {path}")
        } else {
            format!(", {path}")
        };
        return Some((Span::with_root_ctxt(close, close), text));
    }
}

/// `(insertion point, text)` for the derive: `, <path>` before the `)` of an
/// existing source `#[derive(..)]`, else a `#[derive(<path>)]` line before
/// the item (which starts at `struct`/visibility, after its attributes).
fn derive_part(cx: &LateContext<'_>, item: &Item<'_>, path: &str) -> (Span, String) {
    if let Some(part) = extend_derive_part(cx.tcx.sess.source_map(), item, path) {
        return part;
    }
    let indent = snippet_indent(cx.sess(), item.span).unwrap_or_default();
    (
        item.span.shrink_to_lo(),
        format!("#[derive({path})]\n{indent}"),
    )
}

/// `(insertion point, text)` for `#[id]` — before the field's first
/// attribute or at the field itself; `#[id] ` inline for tuple fields,
/// `#[id]\n<indent>` for named ones.
fn id_part(cx: &LateContext<'_>, field: &FieldDef<'_>) -> (Span, String) {
    let point = cx
        .tcx
        .hir_attrs(field.hir_id)
        .iter()
        .filter_map(attr_span)
        .filter(|span| !span.from_expansion())
        .min_by_key(|span| span.lo())
        .unwrap_or(field.span)
        .shrink_to_lo();
    if field.is_positional() {
        (point, "#[id] ".to_string())
    } else {
        let indent = snippet_indent(cx.sess(), point).unwrap_or_default();
        (point, format!("#[id]\n{indent}"))
    }
}

/// `item`'s span extended to the end of its line plus one trailing newline —
/// the span whose deletion removes the item cleanly.
fn delete_span(sm: &SourceMap, span: Span) -> Span {
    // Back up over the line's own indentation so the deletion takes the
    // whole line and leaves no blank line of trailing whitespace behind.
    let bol = sm
        .span_extend_prev_while(span, |c| c == ' ' || c == '\t')
        .unwrap_or(span);
    let eol = sm.span_extend_while(bol, |c| c != '\n').unwrap_or(bol);
    let end = sm.lookup_char_pos(eol.hi()).file.end_position();
    eol.with_hi((eol.hi() + BytePos(1)).min(end))
}

/// All `spans` live in one source file — rustfix applies suggestions per
/// file, so a cross-file fix is demoted to help text.
fn same_file(sm: &SourceMap, spans: impl IntoIterator<Item = Span>) -> bool {
    let mut files = spans
        .into_iter()
        .map(|span| sm.lookup_char_pos(span.lo()).file);
    let Some(first) = files.next() else {
        return true;
    };
    files.all(|file| Arc::ptr_eq(&file, &first))
}

/// The trait items of `Identifiable` — `(fn id, type Id)` — for matching the
/// impl's items through `trait_item_def_id`.
fn identifiable_items(cx: &LateContext<'_>, trait_did: DefId) -> (Option<DefId>, Option<DefId>) {
    let mut fn_did = None;
    let mut ty_did = None;
    for assoc in cx.tcx.associated_items(trait_did).in_definition_order() {
        match assoc.opt_name() {
            Some(name) if name.as_str() == "id" => fn_did = Some(assoc.def_id),
            Some(name) if name.as_str() == "Id" => ty_did = Some(assoc.def_id),
            _ => {}
        }
    }
    (fn_did, ty_did)
}

/// The struct-side suggestion parts: the derive (extend or create) plus the
/// `#[id]` marker on `field`.
fn struct_parts(
    cx: &LateContext<'_>,
    item: &Item<'_>,
    field: &FieldDef<'_>,
) -> Vec<(Span, String)> {
    vec![
        derive_part(cx, item, derive_path(cx, item)),
        id_part(cx, field),
    ]
}

impl<'tcx> LateLintPass<'tcx> for ManualIdentifiable {
    /// (a) — `impl Identifiable for T` where `T` is a local struct and `id`
    /// returns a field verbatim.
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        let ItemKind::Impl(impl_) = item.kind else {
            return;
        };
        if item.span.from_expansion() || cx.tcx.is_automatically_derived(item.owner_id.to_def_id())
        {
            return;
        }
        let Some(header) = impl_.of_trait else {
            return;
        };
        let Some(trait_did) = header.trait_ref.trait_def_id() else {
            return;
        };
        if !def_path_eq(cx, trait_did, IDENTIFIABLE) {
            return;
        }
        let self_ty = cx
            .tcx
            .type_of(item.owner_id.to_def_id())
            .instantiate_identity()
            .skip_norm_wip();
        let TyKind::Adt(adt, args) = self_ty.kind() else {
            return;
        };
        if !adt.is_struct() || !adt.did().is_local() || !identity_args(args) {
            return;
        }
        let Some(struct_item) = local_struct_item(cx, self_ty) else {
            return;
        };
        let (Some(fn_did), Some(ty_did)) = identifiable_items(cx, trait_did) else {
            return;
        };
        let mut id_body = None;
        let mut id_ty_owner = None;
        for &item_id in impl_.items {
            let impl_item = cx.tcx.hir_impl_item(item_id);
            let ImplItemImplKind::Trait {
                trait_item_def_id: Ok(did),
                ..
            } = impl_item.impl_kind
            else {
                continue;
            };
            match impl_item.kind {
                ImplItemKind::Fn(_, body) if did == fn_did => id_body = Some(body),
                ImplItemKind::Type(_) if did == ty_did => id_ty_owner = Some(impl_item.owner_id),
                _ => {}
            }
        }
        let (Some(body_id), Some(ty_owner)) = (id_body, id_ty_owner) else {
            return;
        };
        let typeck = cx.tcx.typeck_body(body_id);
        let Some(name) = id_field(cx, typeck, cx.tcx.hir_body(body_id)) else {
            return;
        };
        let Some(field) = field_by_name(struct_item, name) else {
            return;
        };
        if field.span.from_expansion() {
            return;
        }
        // `type Id` must be the field's type — `self.id as u64` is not a
        // field read and a mismatched `Id` is a different trait contract.
        let field_ty = cx
            .tcx
            .type_of(field.def_id.to_def_id())
            .instantiate(cx.tcx, args)
            .skip_norm_wip();
        let id_ty = cx
            .tcx
            .type_of(ty_owner.to_def_id())
            .instantiate_identity()
            .skip_norm_wip();
        if id_ty != field_ty {
            return;
        }
        span_lint_and_then(
            cx,
            MANUAL_IDENTIFIABLE,
            item.span,
            format!("`Identifiable` can be derived — mark `{name}` with `#[id]` and drop the impl"),
            |diag| {
                let sm = cx.tcx.sess.source_map();
                let mut parts = struct_parts(cx, struct_item, field);
                parts.push((delete_span(sm, item.span), String::new()));
                if same_file(sm, parts.iter().map(|(span, _)| *span)) {
                    diag.multipart_suggestion(
                        "derive `Identifiable` and mark the field with `#[id]`",
                        parts,
                        Applicability::MaybeIncorrect,
                    );
                } else {
                    diag.help(format!(
                        "mark `{name}` with `#[id]` on `{}` and delete this impl",
                        cx.tcx.item_name(adt.did())
                    ));
                }
            },
        );
    }

    /// (b) — `x.use_id(|v| v.f)` / `x.self_id()` on a local struct.
    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion() {
            return;
        }
        let Some(callee) = call_def_id(cx.typeck_results(), expr) else {
            return;
        };
        let callee = implemented_trait_item(cx.tcx, callee);
        let args = call_args(expr);
        let mark = if def_path_eq(cx, callee, USE_ID) {
            let [_, func] = args.as_slice() else {
                return;
            };
            closure_mark(cx, func)
        } else if def_path_eq(cx, callee, SELF_ID) {
            Some(Mark::Whole)
        } else {
            None
        };
        let Some(mark) = mark else {
            return;
        };
        let Some(&receiver) = args.first() else {
            return;
        };
        let receiver_ty = cx.typeck_results().expr_ty(receiver);
        let Some(struct_item) = local_struct_item(cx, receiver_ty) else {
            return;
        };
        // `use_id`/`self_id` is the only option for foreign types; on a type
        // that is already `Identifiable` the wrapper is a different concern.
        let Some(trait_did) = lookup_path_str(
            cx.tcx,
            PathNS::Type,
            "waterui_core::foundation::id::Identifiable",
        )
        .first()
        .copied() else {
            return;
        };
        if implements_trait(cx, receiver_ty, trait_did, &[]) {
            return;
        }
        let field = match mark {
            Mark::Field(name) => field_by_name(struct_item, name),
            Mark::Whole => single_field(struct_item),
        };
        let Some(field) = field else {
            return;
        };
        if field.span.from_expansion() {
            return;
        }
        let name = cx.tcx.item_name(struct_item.owner_id.to_def_id());
        span_lint_and_then(
            cx,
            MANUAL_IDENTIFIABLE,
            expr.span,
            format!("`{name}` can derive `Identifiable` — `use_id`/`self_id` wrap foreign types"),
            |diag| {
                let sm = cx.tcx.sess.source_map();
                let parts = struct_parts(cx, struct_item, field);
                if same_file(sm, parts.iter().map(|(span, _)| *span).chain([expr.span]))
                    && let Some(receiver) = snippet_opt(cx.sess(), receiver.span)
                {
                    let mut parts = parts;
                    parts.push((expr.span, receiver));
                    diag.multipart_suggestion(
                        format!("derive `Identifiable` on `{name}` and use the value directly"),
                        parts,
                        Applicability::MaybeIncorrect,
                    );
                } else {
                    diag.help(format!(
                        "derive `Identifiable` on `{name}`, mark `{}` with `#[id]`, and use the receiver directly",
                        field.ident.name
                    ));
                }
            },
        );
    }
}
