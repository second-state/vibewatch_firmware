//! Heap trace window (DEBUG TOOL, unlinked by default).
//!
//! Records every allocation that stays alive inside a code window and dumps
//! them with sizes — used to attribute internal RAM usage (the 45 KB LCD /
//! GIF investigation on try-tailscale).
//!
//! To enable:
//! 1. Uncomment `mod heap_trace;` in main.rs and the call sites marked
//!    `heap_trace` in main.rs / ui.rs.
//! 2. Re-add to sdkconfig.defaults:
//!        CONFIG_HEAP_TRACING_STANDALONE=y
//!        CONFIG_HEAP_TRACING_STACK_DEPTH=2
//!    WARNING: the HeapTraceRecord layout below MUST match
//!    CONFIG_HEAP_TRACING_STACK_DEPTH (the 2-word version caused a
//!    TLSF heap-corruption crash when mismatched).
//! 3. Tracing is NOT ISR-safe: expect watchdog panics under heavy
//!    display/audio traffic; stop the window before long prints.

// --- Heap trace window (CONFIG_HEAP_TRACING_STANDALONE must be enabled) ---
// Mirrors heap_trace_record_t. MUST match CONFIG_HEAP_TRACING_STACK_DEPTH
// (currently 2) and HEAP_TRACE_HASH_MAP=n, or the trace buffer overflows
// and corrupts the heap (TLSF assert in remove_free_block).
#[repr(C)]
pub struct HeapTraceRecord {
    ccount: u32,
    address: *mut core::ffi::c_void,
    size: usize,
    alloced_by: [*mut core::ffi::c_void; 2],
    freed_by: [*mut core::ffi::c_void; 2],
    tailq: [*mut core::ffi::c_void; 2],
}

unsafe extern "C" {
    fn heap_trace_init_standalone(buf: *mut HeapTraceRecord, num_records: usize) -> i32;
    fn heap_trace_start(mode: u32) -> i32; // 1 = HEAP_TRACE_LEAKS
    fn heap_trace_stop() -> i32;
    fn heap_trace_dump();
}

/// Begins recording allocations that stay alive (leak-style).
///
/// Returns a guard whose Drop stops tracing and dumps every still-allocated
/// block with its size, so a memory window can be attributed precisely.
pub fn heap_trace_window_start(record_capacity: usize) {
    #[allow(unused_mut)]
    let mut records: Vec<HeapTraceRecord> = (0..record_capacity)
        .map(|_| HeapTraceRecord {
            ccount: 0,
            address: core::ptr::null_mut(),
            size: 0,
            alloced_by: [core::ptr::null_mut(); 2],
            freed_by: [core::ptr::null_mut(); 2],
            tailq: [core::ptr::null_mut(); 2],
        })
        .collect();
    // SAFETY: buffer outlives the trace window (leaked here on purpose).
    let records = Box::leak(records.into_boxed_slice());
    // SAFETY: plain ESP-IDF heap trace calls with a valid buffer.
    unsafe {
        heap_trace_init_standalone(records.as_mut_ptr(), record_capacity);
        heap_trace_start(1);
    }
}

/// Stops the trace and dumps all allocations still alive since the start.
pub fn heap_trace_window_stop() {
    // SAFETY: plain ESP-IDF heap trace calls.
    unsafe {
        heap_trace_stop();
        heap_trace_dump();
    }
}

/// Stops and dumps the trace window at most once (for one-shot experiments).
pub fn heap_trace_window_stop_once() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static STOPPED: AtomicBool = AtomicBool::new(false);
    if !STOPPED.swap(true, Ordering::Relaxed) {
        heap_trace_window_stop();
    }
}
