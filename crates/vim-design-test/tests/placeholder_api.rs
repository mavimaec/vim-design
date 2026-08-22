//! Integration tests exercising the placeholder vim-design-lib API from
//! outside the crate, the way real consumers will.

use vim_design_lib::{Document, EntityId, IdAllocator, VimStatus};

#[test]
fn document_lifecycle_and_id_allocation() {
    let mut doc = Document::new();
    assert_eq!(doc.generation(), 0);

    let first = doc.allocate_id();
    let second = doc.allocate_id();
    assert_ne!(first, EntityId::INVALID);
    assert!(second > first, "ids must be monotonic");
    assert_eq!(doc.generation(), 2);
}

#[test]
fn independent_documents_do_not_share_state() {
    let mut a = Document::new();
    let mut b = Document::new();
    let ia = a.allocate_id();
    let ib = b.allocate_id();
    // Per-document allocators start at the same origin.
    assert_eq!(ia, ib);
    assert_eq!(a.generation(), 1);
    assert_eq!(b.generation(), 1);
}

#[test]
fn id_allocator_never_returns_invalid() {
    let mut alloc = IdAllocator::new();
    for _ in 0..1000 {
        assert_ne!(alloc.allocate(), EntityId::INVALID);
    }
}

#[test]
fn status_ok_is_zero() {
    assert!(VimStatus::Ok.is_ok());
    assert_eq!(VimStatus::Ok as u32, 0);
    assert!(!VimStatus::InternalPanic.is_ok());
}

#[test]
fn kernel_probe_produces_geometry() {
    // Proves the truck kernel links and runs on the native target.
    let desc = vim_design_lib::kernel::probe();
    assert!(desc.contains("truck vertex created"), "got: {desc}");
}
