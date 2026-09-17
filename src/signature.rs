//! Function/method signature queries shared by the signature-level lints:
//! the written-visibility check (`public_signature`), the export exemption
//! (`exported`), and the `LocalDefId` → `(decl, body, trait member)` lookup
//! (`signature_of`).

use clippy_utils::source::snippet_opt;
use rustc_abi::ExternAbi;
use rustc_hir::attrs::AttributeKind;
use rustc_hir::def_id::{DefId, LocalDefId};
use rustc_hir::{
    Attribute, Body, Constness, FnHeader, ImplItemKind, ItemKind, Node, QPath, TraitFn,
    TraitItemKind, TyKind,
};
use rustc_lint::LateContext;
use rustc_middle::ty::{self, Ty, TyCtxt, Unnormalized};
use rustc_span::Span;

/// `did`'s parameters as `(index, normalized type)` — `fn_sig` inputs under
/// identity instantiation with type aliases normalized away, so a parameter
/// written through a `type` alias is seen as its target.
pub(crate) fn normalized_inputs<'tcx>(cx: &LateContext<'tcx>, did: DefId) -> Vec<(u32, Ty<'tcx>)> {
    let env = ty::TypingEnv::post_analysis(cx.tcx, did);
    cx.tcx
        .fn_sig(did)
        .instantiate_identity()
        .skip_norm_wip()
        .skip_binder()
        .inputs()
        .iter()
        .enumerate()
        .map(|(index, &input)| {
            (
                index as u32,
                cx.tcx
                    .try_normalize_erasing_regions(env, Unnormalized::new_wip(input))
                    .unwrap_or(input),
            )
        })
        .collect()
}

/// Whether an item — from `check_item`/`check_impl_item`/`check_trait_item`
/// — is a signature the caller may rewrite: Rust ABI (`extern` signatures
/// cannot change shape), non-`const` (the inserted calls can never run in a
/// const body), and not exported. `has_params` runs only when those gates
/// pass, so a lint's parameter scan is skipped for exempt items.
pub(crate) fn collectable(
    cx: &LateContext<'_>,
    did: LocalDefId,
    attrs: &[Attribute],
    header: FnHeader,
    has_params: impl FnOnce(&LateContext<'_>, DefId) -> bool,
) -> bool {
    header.abi == ExternAbi::Rust
        && header.constness == Constness::NotConst
        && !exported(attrs)
        && has_params(cx, did.to_def_id())
}

/// `#[no_mangle]`/`#[export_name]` — an exported signature cannot change
/// shape (e.g. an `impl Trait` parameter has no C ABI).
pub(crate) fn exported(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        matches!(
            attr,
            Attribute::Parsed(AttributeKind::NoMangle(_) | AttributeKind::ExportName { .. })
        )
    })
}

/// Whether `did`'s signature carries a written visibility qualifier and the
/// item is not nested inside a body, where no qualifier can reach a caller
/// anyway. A trait member cannot be qualified itself; it borrows its
/// trait's qualifier. An inherent method is nameable only while its self
/// type is, so a `pub fn` on a private `struct` stays under the private
/// call-site rule.
pub(crate) fn public_signature(cx: &LateContext<'_>, did: LocalDefId) -> bool {
    let tcx = cx.tcx;
    let hir_id = tcx.local_def_id_to_hir_id(did);
    let nested = tcx
        .hir_parent_id_iter(hir_id)
        .any(|id| tcx.hir_node(id).associated_body().is_some());
    if nested {
        return false;
    }
    let node = match tcx.hir_node(hir_id) {
        Node::TraitItem(_) => {
            let Some(owner) = tcx.parent(did.to_def_id()).as_local() else {
                return false;
            };
            tcx.hir_node(tcx.local_def_id_to_hir_id(owner))
        }
        node => node,
    };
    match node {
        Node::Item(item) => written_public(cx, item.vis_span),
        Node::ImplItem(item) => {
            item.vis_span().is_some_and(|span| written_public(cx, span))
                && impl_self_public(cx, did)
        }
        _ => false,
    }
}

/// Whether a `vis_span` is a real qualifier — `pub`, `pub(crate)`,
/// `pub(super)`, `pub(in ..)`. `pub(self)`/`pub(in self)` are private no
/// matter where they are written, and an unqualified item keeps the
/// parser's zero-width placeholder span, which `is_empty` detects.
/// (`tcx.local_visibility` cannot replace the written check: it merges
/// `pub(crate)` at the crate root with private.)
fn written_public(cx: &LateContext<'_>, span: Span) -> bool {
    if span.is_empty() {
        return false;
    }
    let squashed = snippet_opt(cx, span).map(|text| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    });
    !matches!(squashed.as_deref(), Some("pub(self)" | "pub(inself)"))
}

/// Whether `did` — an inherent impl item — sits on an ADT that carries its
/// own written qualifier. Non-ADT and foreign self types cannot be inherent
/// receivers anyway; they count as private.
fn impl_self_public(cx: &LateContext<'_>, did: LocalDefId) -> bool {
    let tcx = cx.tcx;
    let Some(impl_did) = tcx.parent(did.to_def_id()).as_local() else {
        return false;
    };
    let Node::Item(item) = tcx.hir_node(tcx.local_def_id_to_hir_id(impl_did)) else {
        return false;
    };
    let ItemKind::Impl(impl_) = item.kind else {
        return false;
    };
    let TyKind::Path(QPath::Resolved(None, path)) = impl_.self_ty.kind else {
        return false;
    };
    let Some(adt) = path.res.opt_def_id().and_then(DefId::as_local) else {
        return false;
    };
    match tcx.hir_node(tcx.local_def_id_to_hir_id(adt)) {
        Node::Item(item) => written_public(cx, item.vis_span),
        _ => false,
    }
}

/// `(decl, body, is_trait_member)` for a collected function.
pub(crate) fn signature_of<'hir>(
    tcx: TyCtxt<'hir>,
    did: LocalDefId,
) -> Option<(
    &'hir rustc_hir::FnDecl<'hir>,
    Option<&'hir Body<'hir>>,
    bool,
)> {
    match tcx.hir_node_by_def_id(did) {
        Node::Item(item) => match item.kind {
            ItemKind::Fn { sig, body, .. } => Some((sig.decl, Some(tcx.hir_body(body)), false)),
            _ => None,
        },
        Node::ImplItem(item) => match item.kind {
            ImplItemKind::Fn(sig, body) => Some((sig.decl, Some(tcx.hir_body(body)), false)),
            _ => None,
        },
        Node::TraitItem(item) => match item.kind {
            TraitItemKind::Fn(sig, TraitFn::Provided(body)) => {
                Some((sig.decl, Some(tcx.hir_body(body)), true))
            }
            TraitItemKind::Fn(sig, TraitFn::Required(..)) => Some((sig.decl, None, true)),
            _ => None,
        },
        _ => None,
    }
}
