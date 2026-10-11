use std::sync::{Mutex, MutexGuard};

/// Locks a mutex, treating poisoning as recoverable: every guarded value here
/// is plain state that stays valid if another thread panicked while holding it.
pub(crate) trait MutexExt<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
