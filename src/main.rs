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
    state: AtomicU32,
    value: cell::UnsafeCell<T>,
}

unsafe impl<T> Send for RwLock<T> where T: Send {}
unsafe impl<T> Sync for RwLock<T> where T: Sync {}

struct ReadGuard<'a, T> {
    rwlock: &'a RwLock<T>,
}

impl<T> Deref for ReadGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.rwlock.value.get() }
    }
}

impl<T> Drop for ReadGuard<'_, T> {
    fn drop(&mut self) {
        let state = &self.rwlock.state;
        let prev = state.fetch_sub(1, Ordering::Release);
        assert!(prev != u32::MAX);
        if prev == 1 {
            atomic_wait::wake_one(state);
        }
    }
}

struct WriteGuard<'a, T> {
    rwlock: &'a mut RwLock<T>,
}

impl<T> Deref for WriteGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.rwlock.value.get() }
    }
}

impl<T> DerefMut for WriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.rwlock.value.get_mut()
    }
}

impl<T> Drop for WriteGuard<'_, T> {
    fn drop(&mut self) {
        let state = &self.rwlock.state;
        assert!(state.load(Ordering::Relaxed) == u32::MAX);
        state.store(0, Ordering::Release);
        atomic_wait::wake_one(state);
    }
}

impl<T> RwLock<T> {
    /// Cap the total number of concurrent readers so as not to overflow.  (This
    /// would be unnecessary if we could wait/wake on an `AtomicUsize`.)
    const MAX_READERS: u32 = 1u32 << 8;

    fn new(t: T) -> Self {
        RwLock {
            state: AtomicU32::new(0),
            value: cell::UnsafeCell::new(t),
        }
    }

    /// Acquire a shared reference to the `T` behind the lock.
    fn get(rwlock: &Self) -> ReadGuard<T> {
        let state = &rwlock.state;
        loop {
            let s = state.load(Ordering::Relaxed);
            if s == u32::MAX {
                atomic_wait::wait(state, s);
                continue;
            }

            if s == Self::MAX_READERS {
                atomic_wait::wait(state, Self::MAX_READERS);
                continue;
            }

            if state
                .compare_exchange(s, s + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return ReadGuard { rwlock };
            }
        }
    }

    /// Acquire an exclusive reference to the `T` behind the lock.
    fn get_mut(rwlock: &Self) -> WriteGuard<T> {
        let state = &rwlock.state;
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
                return WriteGuard {
                    rwlock: unsafe { &mut *(rwlock as *const Self as *mut Self) },
                };
            }
        }
    }
}

fn main() {
    let rwlock = RwLock::new(0);

    let res = thread::scope(|scope| {
        (0..16)
            .map(|i| {
                let rwlock = Arc::new(&rwlock);

                scope.spawn(move || {
                    let ms = rand::thread_rng().gen_range(0..64);

                    thread::sleep(Duration::from_millis(ms));

                    let (act, desc);
                    if i % 4 == 0 {
                        act = "write";

                        let mut g = RwLock::get_mut(&rwlock);
                        let initially = *g;
                        *g += 1;
                        let after = *g;

                        desc = format!("{initially} - {after}");
                    } else {
                        act = "read";

                        let g = RwLock::get(&rwlock);

                        let v = *g;
                        desc = format!("{v}");
                    }

                    format!("{act}: {desc}")
                })
            })
            .map(thread::ScopedJoinHandle::join)
            .map(Result::unwrap)
            .collect::<Vec<_>>()
    });

    for line in res {
        println!("{line}");
    }
}
