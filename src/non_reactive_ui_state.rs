use clippy_utils::diagnostics::span_lint_and_help;
use clippy_utils::res::{MaybeDef, MaybeResPath};
use rustc_ast::Mutability;
use rustc_data_structures::fx::FxHashMap;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::hir_id::HirId;
use rustc_hir::intravisit::{self, Visitor, nested_filter};
use rustc_hir::{Closure, Expr, ExprKind, ImplPolarity, Item, ItemKind, QPath};
use rustc_lint::{LateContext, LateLintPass};
use rustc_middle::hir::place::PlaceBase;
use rustc_middle::ty::{Ty, TyKind};
use rustc_session::declare_lint_pass;
use rustc_span::{Span, sym};

use crate::anyview::peel;
use crate::def_path::def_path_eq;
use crate::param_bounds::{HANDLER_PARAM_BOUNDS, call_arg_bounds, call_args};

declare_waterui_lint! {
    /// ### What it does
    ///
    /// Flags non-reactive mutable state used where WaterUI expects a
    /// `Binding`: a handler closure — `button(..).action(..)`,
    /// `action_async(..)`, `.on_tap(..)`, `.gesture(..)` — that mutates a
    /// captured `RefCell`/`Cell`/`Mutex`/`RwLock`/atomic (through `Rc`/`Arc`
    /// or not) or touches a `static mut`/`thread_local!`, and a field of one
    /// of those types on a type implementing `View`.
    ///
    /// ### Why is this bad?
    ///
    /// Mutating interior-mutability state never notifies the reactive graph:
    /// the cell holds the new value, but no signal fires, so the view keeps
    /// showing the value it first read. This is the classic "why does the UI
    /// not update" bug when coming from other frameworks.
    ///
    /// ### Example
    ///
    /// ```rust,ignore
    /// let count = Rc::new(RefCell::new(0));
    /// button("+").action(move || *count.borrow_mut() += 1);
    /// ```
    ///
    /// Keep UI state in a `Binding` and inject it into the handler instead:
    ///
    /// ```rust,ignore
    /// let count: Binding<i32> = binding(0);
    /// button("+")
    ///     .action(|State(count): State<Binding<i32>>| count.with_mut(|v| *v += 1))
    ///     .state(&count);
    /// ```
    pub NON_REACTIVE_UI_STATE,
    suspicious,
    "interior-mutability state the reactive graph never observes"
}

declare_lint_pass!(NonReactiveUiState => [NON_REACTIVE_UI_STATE]);

const VIEW: &[&str] = &["waterui_core", "ui", "view", "View"];

const HELP: &str = "keep UI state in a `Binding<T>` (or a `ReactiveList<T>` for sequences); the reactive graph is notified by `set`, not by interior mutability";

/// The non-reactive state kinds the lint flags. `Mutex`/`RwLock`/`Atomic`
/// are shared-state variants of the same "mutation nobody observes" shape.
#[derive(Clone, Copy)]
enum CellKind {
    RefCell,
    Cell,
    Mutex,
    RwLock,
    /// `core::sync::atomic::Atomic<T>` — `AtomicU32` and friends are aliases
    /// of the one generic `Atomic` ADT.
    Atomic,
}

impl CellKind {
    /// The name the diagnostic renders — `Atomic` reports the peeled type's
    /// argument (`Atomic<u32>`) so the reader sees the concrete atomic.
    fn name(self, ty: Ty<'_>) -> String {
        match self {
            Self::RefCell => "RefCell".into(),
            Self::Cell => "Cell".into(),
            Self::Mutex => "Mutex".into(),
            Self::RwLock => "RwLock".into(),
            Self::Atomic => match ty.kind() {
                TyKind::Adt(_, args) => format!("Atomic<{}>", args.type_at(0)),
                _ => "Atomic".into(),
            },
        }
    }

    /// Whether a method named `method` mutates the state. The receiver is
    /// already known to be this kind, so the name alone identifies the call.
    fn mutates(self, method: &str) -> bool {
        match self {
            Self::RefCell => method == "borrow_mut",
            Self::Cell => matches!(method, "set" | "replace" | "take"),
            Self::Mutex => method == "lock",
            Self::RwLock => method == "write",
            Self::Atomic => {
                method == "store"
                    || method == "swap"
                    || method.starts_with("fetch_")
                    || method.starts_with("compare_exchange")
            }
        }
    }
}

/// `ty` with `&`-references and `Rc`/`Arc` layers removed — a
/// `Rc<RefCell<T>>` is the same non-reactive state as a bare `RefCell<T>`.
fn peel_shared<'tcx>(cx: &LateContext<'tcx>, mut ty: Ty<'tcx>) -> Ty<'tcx> {
    loop {
        ty = ty.peel_refs();
        match ty.kind() {
            TyKind::Adt(adt, args)
                if adt.is_diag_item(cx, sym::Rc) || adt.is_diag_item(cx, sym::Arc) =>
            {
                ty = args.type_at(0);
            }
            _ => return ty,
        }
    }
}

/// The `CellKind` of `ty` — refs/`Rc`/`Arc` peeled — with the peeled type
/// for the diagnostic's name.
fn cell_kind<'tcx>(cx: &LateContext<'tcx>, ty: Ty<'tcx>) -> Option<(CellKind, Ty<'tcx>)> {
    let ty = peel_shared(cx, ty);
    let TyKind::Adt(adt, _) = ty.kind() else {
        return None;
    };
    let kind = if adt.is_diag_item(cx, sym::RefCell) {
        CellKind::RefCell
    } else if adt.is_diag_item(cx, sym::Cell) {
        CellKind::Cell
    } else if adt.is_diag_item(cx, sym::Mutex) {
        CellKind::Mutex
    } else if adt.is_diag_item(cx, sym::RwLock) {
        CellKind::RwLock
    } else if adt.is_diag_item(cx, sym::Atomic) {
        CellKind::Atomic
    } else {
        return None;
    };
    Some((kind, ty))
}

/// Finds the offenses inside one handler closure's body: mutating method
/// calls on captured state cells, `static mut` access, and `thread_local!`
/// access. `NestedFilter::None` keeps the walk on this body — nested
/// closures and const blocks are other bodies whose own captures decide.
struct HandlerBody<'a, 'tcx> {
    cx: &'a LateContext<'tcx>,
    /// Captured local `HirId` → `(kind, diagnostic name)`, for each captured
    /// state cell.
    captured: FxHashMap<HirId, (CellKind, String)>,
    /// `(span, name)` per offense found.
    hits: Vec<(Span, String)>,
}

impl<'tcx> Visitor<'tcx> for HandlerBody<'_, 'tcx> {
    type NestedFilter = nested_filter::None;

    fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
        match expr.kind {
            ExprKind::MethodCall(segment, receiver, ..) => {
                let mut receiver = receiver;
                while let ExprKind::AddrOf(.., inner) = receiver.kind {
                    receiver = inner;
                }
                if let Some(&(kind, ref name)) = receiver
                    .res_local_id()
                    .and_then(|local| self.captured.get(&local))
                    && kind.mutates(segment.ident.name.as_str())
                {
                    self.hits.push((expr.span, name.clone()));
                }
            }
            ExprKind::Path(QPath::Resolved(_, path)) => match path.res {
                Res::Def(
                    DefKind::Static {
                        mutability: Mutability::Mut,
                        ..
                    },
                    _,
                ) => self.hits.push((expr.span, "static mut".into())),
                // `thread_local!` items resolve as `const`s; their type is
                // `LocalKey<T>` — the handle every read or write goes through.
                Res::Def(DefKind::Static { .. } | DefKind::Const { .. }, did)
                    if self
                        .cx
                        .tcx
                        .type_of(did)
                        .instantiate_identity()
                        .skip_norm_wip()
                        .is_diag_item(self.cx, sym::LocalKey) =>
                {
                    self.hits.push((expr.span, "thread_local!".into()));
                }
                _ => {}
            },
            _ => {}
        }
        intravisit::walk_expr(self, expr);
    }
}

/// (a) — a closure in a `Handler`/`HandlerOnce` position whose captures or
/// body touch non-reactive state.
fn check_handler_closure<'tcx>(cx: &LateContext<'tcx>, closure: &Closure<'tcx>) {
    let mut captured: FxHashMap<HirId, (CellKind, String)> = FxHashMap::default();
    for capture in cx
        .typeck_results()
        .closure_min_captures_flattened(closure.def_id)
    {
        let local = match capture.place.base {
            PlaceBase::Local(id) => id,
            PlaceBase::Upvar(var) => var.var_path.hir_id,
            _ => continue,
        };
        if let Some((kind, ty)) = cell_kind(cx, capture.place.ty()) {
            captured.insert(local, (kind, kind.name(ty)));
        }
    }
    let mut visitor = HandlerBody {
        cx,
        captured,
        hits: Vec::new(),
    };
    visitor.visit_body(cx.tcx.hir_body(closure.body));
    for (span, name) in visitor.hits {
        span_lint_and_help(
            cx,
            NON_REACTIVE_UI_STATE,
            span,
            format!("mutating a `{name}` from a handler never notifies the UI"),
            None,
            HELP,
        );
    }
}

impl<'tcx> LateLintPass<'tcx> for NonReactiveUiState {
    /// (b) — a `RefCell`/`Cell`/`Mutex`/`RwLock`/atomic field on a type that
    /// implements `View`.
    fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx Item<'tcx>) {
        let ItemKind::Impl(impl_) = item.kind else {
            return;
        };
        if item.span.from_expansion() {
            return;
        }
        let Some(header) = impl_.of_trait else {
            return;
        };
        if header.polarity != ImplPolarity::Positive {
            return;
        }
        let Some(trait_did) = header.trait_ref.trait_def_id() else {
            return;
        };
        if !def_path_eq(cx, trait_did, VIEW) {
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
        for field in adt.all_fields() {
            let ty = field.ty(cx.tcx, args).skip_norm_wip();
            if let Some((kind, peeled)) = cell_kind(cx, ty) {
                span_lint_and_help(
                    cx,
                    NON_REACTIVE_UI_STATE,
                    cx.tcx.def_span(field.did),
                    format!(
                        "a `{}` field on a `View` type is not reactive state",
                        kind.name(peeled)
                    ),
                    None,
                    HELP,
                );
            }
        }
    }

    fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
        if expr.span.from_expansion()
            || !matches!(expr.kind, ExprKind::Call(..) | ExprKind::MethodCall(..))
        {
            return;
        }
        let Some((_, per_arg)) = call_arg_bounds(cx, expr, &[HANDLER_PARAM_BOUNDS]) else {
            return;
        };
        for (arg, targets) in call_args(expr).into_iter().zip(per_arg) {
            if !targets.is_empty()
                && let ExprKind::Closure(closure) = peel(arg).kind
            {
                check_handler_closure(cx, closure);
            }
        }
    }
}
