//! Scoped cancellation for supervised captured processes. Workers explicitly inherit the
//! owner's token; unscoped calls retain their existing deadline-only behaviour.

use std::cell::RefCell;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

thread_local! {
    static CURRENT: RefCell<Option<Cancellation>> = const { RefCell::new(None) };
}

/// A shared stop request, polled outside signal handlers by process-tree supervision.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    /// Uses a flag that a signal handler may set without performing cleanup in the handler.
    #[must_use]
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    /// The token installed on this thread, if any. New threads must inherit it explicitly.
    #[must_use]
    pub fn current() -> Option<Self> {
        CURRENT.with(|current| current.borrow().clone())
    }

    /// True after the owner requested shutdown.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Refuses new work after shutdown has begun.
    ///
    /// # Errors
    /// Returns `Interrupted` if the owner requested cancellation.
    pub fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "mission cancelled",
            ))
        } else {
            Ok(())
        }
    }

    /// Applies this token to captured processes on this thread, restoring any outer scope
    /// even if `action` panics. The scope itself never owns or signals a process.
    pub fn scope<T>(&self, action: impl FnOnce() -> T) -> T {
        struct Restore(Option<Cancellation>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|current| *current.borrow_mut() = self.0.take());
            }
        }
        let _restore = Restore(CURRENT.with(|current| current.replace(Some(self.clone()))));
        action()
    }
}
