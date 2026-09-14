//! `nami`'s `Binding` def paths, shared by lints that match binding reads
//! and writes.

/// `nami`'s `Binding<T>` — the writable signal handle.
pub(crate) const BINDING: &[&str] = &["nami", "reactive_core", "binding", "Binding"];

/// `Binding::set` — the inherent write that publishes a new value. Method
/// resolution prefers it over the `CustomBinding`/`BindingImpl` trait
/// methods, so `b.set(..)` on a `Binding` resolves here.
pub(crate) const BINDING_SET: &[&str] = &["nami", "reactive_core", "binding", "Binding", "set"];
