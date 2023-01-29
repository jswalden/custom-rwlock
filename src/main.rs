use atomic_wait;
use rand::Rng;
use std::cell;
use std::ops::{Deref, DerefMut, Drop};
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

struct RwLock<T> {
    /// 0 if there are no readers or writers in existence.  `u32::MAX` if a
    /// single writer is in existence.  1 or more if that many readers are in
    /// existence.
    state: AtomicU32,

    /// The encapsulated value.
    value: cell::UnsafeCell<T>,
}

/// An `RwLock` may only be used on multiple threads if its contained value can
/// be safely sent and accessed across threads.
unsafe impl<T> Sync for RwLock<T> where T: Send + Sync {}

/// Exposes shared access to the value in an `RwLock`.
struct Reader<'a, T> {
    rwlock: &'a RwLock<T>,
}

impl<T> Deref for Reader<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.rwlock.value.get() }
    }
}

impl<T> Drop for Reader<'_, T> {
    fn drop(&mut self) {
        let state = &self.rwlock.state;
        let prev = state.fetch_sub(1, Ordering::Release);
        assert!(prev > 0, "should be dropping an acquired reader");
        assert!(prev != u32::MAX, "shouldn't be dropping when in write mode");
        if prev == 1 || prev == RwLock::<T>::MAX_READERS {
            atomic_wait::wake_one(state);
        }
    }
}

/// Exposes exclusive mutable access to the value in an `RwLock`.
struct Writer<'a, T> {
    rwlock: &'a mut RwLock<T>,
}

impl<T> Deref for Writer<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.rwlock.value.get() }
    }
}

impl<T> DerefMut for Writer<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.rwlock.value.get_mut()
    }
}

impl<T> Drop for Writer<'_, T> {
    fn drop(&mut self) {
        let state = &self.rwlock.state;
        assert!(state.load(Ordering::Relaxed) == u32::MAX);
        state.store(0, Ordering::Release);
        atomic_wait::wake_all(state);
    }
}

impl<T> RwLock<T> {
    /// Cap the total number of concurrent readers to avoid overflow.  If more
    /// readers than this appear, they'll acquire access as prior readers are
    /// dropped.
    ///
    /// Note that this introduces a chance of deadlock under the right
    /// circumstances.
    ///
    /// (This limit would be unnecessary if we could portably wait/wake on an
    /// `AtomicUsize`.)
    const MAX_READERS: u32 = 1u32 << 8;

    /// Make a new `RwLock` wrapping the provided value.
    fn new(t: T) -> Self {
        RwLock {
            state: AtomicU32::new(0),
            value: cell::UnsafeCell::new(t),
        }
    }

    /// Acquire a shared reference to the `T` behind the lock.
    fn read(&self) -> Reader<T> {
        let state = &self.state;
        loop {
            let s = state.load(Ordering::Relaxed);

            // If there's a pending writer, or if we're maxed out on readers,
            // wait til something changes.
            if s == u32::MAX || s == Self::MAX_READERS {
                atomic_wait::wait(state, s);
                continue;
            }

            if state
                .compare_exchange(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return Reader { rwlock: self };
            }
        }
    }

    /// Acquire an exclusive reference to the `T` behind the lock.
    fn write(&self) -> Writer<T> {
        let state = &self.state;
        loop {
            let s = state.load(Ordering::Relaxed);
            if s != 0 {
                atomic_wait::wait(state, s);
                continue;
            }

            if state
                .compare_exchange(0, u32::MAX, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return Writer {
                    rwlock: unsafe { &mut *(self as *const Self as *mut Self) },
                };
            }
        }
    }
}

fn main() {
    let rwlock = RwLock::new(0);

    let res: Vec<_> = thread::scope(|scope| {
        let ts: Vec<_> = (0..16)
            .map(|i| {
                let rwlock = Arc::new(&rwlock);

                enum Action {
                    Reading,
                    Writing,
                }
                use Action::*;

                let action = if i % 4 == 0 { Writing } else { Reading };
                let ms = rand::thread_rng().gen_range(0..64)
                    * match action {
                        Writing => 2,
                        Reading => 1,
                    };

                scope.spawn(move || {
                    thread::sleep(Duration::from_millis(ms));

                    let desc = match action {
                        Writing => {
                            let mut g = rwlock.write();
                            let initially = *g;
                            *g += 1;
                            let after = *g;

                            format!("{initially} -> {after}")
                        }
                        Reading => {
                            let g = rwlock.read();

                            let v = *g;
                            format!("{v}")
                        }
                    };

                    let act = match action {
                        Writing => "write",
                        Reading => "read",
                    };
                    format!("{act}: {desc}")
                })
            })
            .collect();

        ts.into_iter()
            .map(thread::ScopedJoinHandle::join)
            .map(Result::unwrap)
            .collect()
    });

    for line in res {
        println!("{line}");
    }
}
