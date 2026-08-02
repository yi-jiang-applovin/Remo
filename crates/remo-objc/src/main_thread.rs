//! Main-thread dispatch utility using GCD.
//!
//! UIKit calls must happen on the main thread; this module provides
//! `run_on_main_sync` to safely dispatch closures from background threads.
//!
//! The real GCD path is gated on `feature = "uikit"`, not just
//! `target_vendor = "apple"` — matching every other module in this crate
//! (`device_info`, `screenshot`, `view_tree`, `video_encoder`). `dispatch_sync_f`
//! only ever completes if *something* is servicing the main dispatch queue —
//! true inside a real UIKit app (`UIApplicationMain`'s run loop does this for
//! free), but not in a bare `cargo test`/`cargo run` process on macOS, which
//! has no run loop on its main thread by default. Found the hard way: a
//! plain `#[tokio::test]` that exercised this path (indirectly, via
//! `remo-sdk`'s `device_name()`) hung forever, for exactly this reason —
//! `examples/standalone.rs` sidesteps it by calling `dispatch_main()` itself,
//! but a test binary can't do that. Without `uikit`, there is no UIKit to
//! call into anyway, so the stub below just runs the closure inline.

#[cfg(all(target_vendor = "apple", feature = "uikit"))]
mod gcd {
    type DispatchQueue = *const std::ffi::c_void;
    type DispatchFunction = unsafe extern "C" fn(*mut std::ffi::c_void);

    // dispatch_get_main_queue() is a C macro expanding to `&_dispatch_main_q`.
    #[link(name = "System", kind = "dylib")]
    extern "C" {
        static _dispatch_main_q: std::ffi::c_void;
        fn dispatch_sync_f(
            queue: DispatchQueue,
            context: *mut std::ffi::c_void,
            work: DispatchFunction,
        );
        fn pthread_main_np() -> i32;
    }

    fn main_queue() -> DispatchQueue {
        std::ptr::addr_of!(_dispatch_main_q).cast()
    }

    pub fn is_main_thread() -> bool {
        // SAFETY: pthread_main_np is always safe to call.
        unsafe { pthread_main_np() == 1 }
    }

    /// Run a closure synchronously on the main thread.
    ///
    /// If already on the main thread, executes immediately.
    /// Otherwise, dispatches via GCD `dispatch_sync` (blocks until complete).
    pub fn run_on_main_sync<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        if is_main_thread() {
            return f();
        }

        struct Context<F, R> {
            f: Option<F>,
            result: Option<R>,
        }

        unsafe extern "C" fn trampoline<F: FnOnce() -> R, R>(raw: *mut std::ffi::c_void) {
            let ctx = &mut *(raw as *mut Context<F, R>);
            let f = ctx.f.take().unwrap();
            ctx.result = Some(f());
        }

        let mut ctx = Context {
            f: Some(f),
            result: None,
        };

        // SAFETY: dispatch_sync_f blocks the calling thread until the trampoline
        // returns, so `ctx` remains valid for the entire call. The trampoline casts
        // back to the correct Context<F,R> type.
        unsafe {
            dispatch_sync_f(
                main_queue(),
                std::ptr::addr_of_mut!(ctx).cast(),
                trampoline::<F, R>,
            );
        }

        ctx.result.unwrap()
    }
}

#[cfg(not(all(target_vendor = "apple", feature = "uikit")))]
mod gcd {
    pub fn is_main_thread() -> bool {
        false
    }

    pub fn run_on_main_sync<F, R>(f: F) -> R
    where
        F: FnOnce() -> R,
    {
        f()
    }
}

pub use gcd::{is_main_thread, run_on_main_sync};
