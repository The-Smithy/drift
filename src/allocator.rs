// musl's allocator contends badly across threads, which made parallel indexing about 3x slower.
// Most allocations come from tree-sitter's C code, so the Rust allocator alone doesn't help.

use std::ffi::c_void;
use std::sync::Once;

use libmimalloc_sys::{mi_free, mi_malloc, mi_realloc, mi_zalloc};
use tree_sitter::{Allocator, set_allocator};

#[global_allocator]
static RUST_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

// tree-sitter's allocator must never return null for a non-zero size, so running out of memory aborts.
fn or_abort(pointer: *mut c_void, size: usize) -> *mut c_void {
    if pointer.is_null() && size != 0 {
        std::process::abort();
    }
    pointer
}

unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    or_abort(unsafe { mi_malloc(size) }, size)
}

unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
    let Some(total) = count.checked_mul(size) else {
        std::process::abort();
    };
    or_abort(unsafe { mi_zalloc(total) }, total)
}

unsafe extern "C" fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
    or_abort(unsafe { mi_realloc(pointer, size) }, size)
}

unsafe extern "C" fn free(pointer: *mut c_void) {
    unsafe { mi_free(pointer) }
}

pub fn use_mimalloc_in_tree_sitter() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let allocator = Allocator {
            malloc,
            calloc,
            realloc,
            free,
        };
        // Safe because `parse` calls this before any other tree-sitter API, and `Once` runs it just once.
        unsafe { set_allocator(Some(allocator)) };
    });
}
