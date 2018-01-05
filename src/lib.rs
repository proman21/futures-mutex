//! A Mutex for the Future(s)
//!
//! API is similar to [`futures::sync::BiLock`](https://docs.rs/futures/0.1.11/futures/sync/struct.BiLock.html)
//! However, it can be cloned into as many handles as desired.
//!
//! ```
//! extern crate futures;
//! extern crate futures_mutex;
//!
//! use futures::{Future, Poll, Async};
//! use futures_mutex::FutMutex;
//!
//! struct AddTwo {
//!     lock: FutMutex<usize>
//! }
//!
//! impl Future for AddTwo {
//!     type Item = usize;
//!     type Error = ();
//!     fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
//!         match self.lock.poll_lock() {
//!             Async::Ready(mut g) => {
//!                 *g += 2;
//!                 Ok(Async::Ready(*g))
//!             },
//!             Async::NotReady => Ok(Async::NotReady)
//!         }
//!     }
//! }
//!
//! fn main() {
//!     let lock1: FutMutex<usize> = FutMutex::new(0);
//!     let lock2 = lock1.clone();
//!
//!     let future = AddTwo { lock: lock2 };
//!
//!     // This future will return the current value and the recovered lock.
//!     let used_lock = lock1.lock().map(|b| (*b, b.unlock()));
//!
//!     let _ = future.join(used_lock).map(|(add_two, (value, _))| {
//!         assert_eq!(add_two, value);
//!     }).wait().unwrap();
//! }
//! ```

extern crate futures;
extern crate crossbeam;

use std::ops::{Deref, DerefMut};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{Ordering, AtomicBool};
use std::cell::UnsafeCell;
use crossbeam::sync::MsQueue;
use futures::task::{current, Task};
use futures::{Future, Poll, Async};

#[derive(Debug)]
struct Inner<T> {
    wait_queue: MsQueue<Task>,
    locked: AtomicBool,
    data: UnsafeCell<T>
}

impl<T> Drop for Inner<T> {
    fn drop(&mut self) {
        assert!(!self.locked.load(Ordering::SeqCst))
    }
}

unsafe impl<T: Send> Send for Inner<T> {}
unsafe impl<T: Send> Sync for Inner<T> {}

/// A Mutex designed for use inside Futures. Works like `BiLock<T>` from the `futures` crate, but
/// with more than 2 handles.
///
/// **THIS IS NOT A GENRAL PURPOSE MUTEX! IF YOU CALL `poll_lock` OR `lock` OUTSIDE THE CONTEXT OF A TASK, IT WILL PANIC AND EAT YOUR LAUNDRY.**
///
/// This type provides a Mutex that will track tasks that are requesting the Mutex, and will unpark
/// them in the order they request the lock.
///
/// *As of now, there is no strong guarantee that a particular handle of the lock won't be starved. Hopefully the use of the queue will prevent this, but I haven't tried to test that.*
#[derive(Debug)]
#[deprecated(since = "0.2.1", note = "Crate is no longer being maintained")]
pub struct FutMutex<T> {
    inner: Arc<Inner<T>>
}

impl<T> FutMutex<T> {
    /// Create a new FutMutex wrapping around a value `t`
    pub fn new(t: T) -> FutMutex<T> {
        let inner = Arc::new(Inner {
            wait_queue: MsQueue::new(),
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(t)
        });

        FutMutex {
            inner: inner
        }
    }

    /// This will attempt a non-blocking lock on the mutex, returning `Async::NotReady` if it
    /// can't be acquired.
    ///
    /// This function will return immediatly with a `FutMutexGuard` if the lock was acquired
    /// successfully. When it drops, the lock will be unlocked.
    ///
    /// If it can't acquire the lock, it will schedule the current task to be unparked when it
    /// might be able lock the mutex again.
    ///
    /// # Panics
    ///
    /// This function will panic if called outside the context of a future's task.
    pub fn poll_lock(&self) -> Async<FutMutexGuard<T>> {
        if self.inner.locked.compare_and_swap(false, true, Ordering::SeqCst) {
            self.inner.wait_queue.push(current());
            Async::NotReady
        } else {
            Async::Ready(FutMutexGuard{ inner: self })
        }
    }

    /// Convert this lock into a future that resolves to a guard that allows access to the data.
    /// This function returns `FutMutexAcquire<T>`, which resolves to a `FutMutexAcquired<T>`
    /// guard type.
    ///
    /// The returned future will never return an error.
    pub fn lock(self) -> FutMutexAcquire<T> {
        FutMutexAcquire {
            inner: self
        }
    }


    /// We unlock the mutex and wait for someone to lock. We try and unpark as many tasks as we
    /// can to prevents dead tasks from deadlocking the mutex, or tasks that have finished their
    /// critical section and were awakened.
    fn unlock(&self) {
        if !self.inner.locked.swap(false, Ordering::SeqCst) {
            panic!("Tried to unlock an already unlocked FutMutex, something has gone terribly wrong");
        }

        while !self.inner.locked.load(Ordering::SeqCst) {
            match self.inner.wait_queue.try_pop() {
                Some(task) => task.notify(),
                None => return
            }
        }
    }
}

impl<T> Clone for FutMutex<T> {
    fn clone(&self) -> FutMutex<T> {
        FutMutex {
            inner: self.inner.clone()
        }
    }
}

/// Returned RAII guard from the `poll_lock` method.
///
/// This structure acts as a sentinel to the data in the `FutMutex<T>` itself,
/// implementing `Deref` and `DerefMut` to `T`. When dropped, the lock will be
/// unlocked.
#[derive(Debug)]
#[deprecated(since = "0.2.1", note = "Crate is no longer being maintained")]
pub struct FutMutexGuard<'a, T: 'a> {
    inner: &'a FutMutex<T>
}

impl<'a, T> Deref for FutMutexGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner.inner.data.get() }
    }
}

impl<'a, T> DerefMut for FutMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.inner.inner.data.get() }
    }
}

impl<'a, T> Drop for FutMutexGuard<'a, T> {
    fn drop(&mut self) {
        self.inner.unlock();
    }
}

/// Future returned by `FutMutex::lock` which resolves to a guard when a lock is acquired.
#[derive(Debug)]
#[deprecated(since = "0.2.1", note = "Crate is no longer being maintained")]
pub struct FutMutexAcquire<T> {
    inner: FutMutex<T>
}

impl<T> Future for FutMutexAcquire<T> {
    type Item = FutMutexAcquired<T>;
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match self.inner.poll_lock() {
            Async::Ready(r) => {
                mem::forget(r);
                Ok(FutMutexAcquired {
                    inner: FutMutex{ inner: self.inner.inner.clone() }
                }.into())
            },
            Async::NotReady => Ok(Async::NotReady)
        }
    }
}

#[derive(Debug)]
/// Resolved value of `FutMutexAcquire<T>` future
///
/// This value works like `FutMutexGuard<T>`, providing a RAII guard to the value `T` through
/// `Deref` and `DerefMut`. Will unlock the lock when dropped; the original `FutMutex` can be
/// recovered with `unlock()`.
#[deprecated(since = "0.2.1", note = "Crate is no longer being maintained")]
pub struct FutMutexAcquired<T> {
    inner: FutMutex<T>
}

impl<T> FutMutexAcquired<T> {
    pub fn unlock(self) -> FutMutex<T> {
        FutMutex {
            inner: self.inner.inner.clone()
        }
    }
}

impl<T> Deref for FutMutexAcquired<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.inner.inner.data.get() }
    }
}

impl<T> DerefMut for FutMutexAcquired<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.inner.inner.data.get() }
    }
}

impl<T> Drop for FutMutexAcquired<T> {
    fn drop(&mut self) {
        self.inner.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::{self, Notify};
    use futures::future;
    use futures::stream::{self, Stream};
    use std::thread;

    pub fn unpark_noop() -> Arc<Notify> {
        struct Foo;

        impl Notify for Foo {
            fn notify(&self, id: usize) {}
        }

        Arc::new(Foo)
    }

    #[test]
    fn simple() {
        let future = future::lazy(|| {
            let lock1 = FutMutex::new(1);
            let lock2 = lock1.clone();
            let lock3 = lock1.clone();

            let mut guard = match lock1.poll_lock() {
                Async::Ready(g) => g,
                Async::NotReady => panic!("poll not ready"),
            };
            assert_eq!(*guard, 1);
            *guard = 2;

            assert!(lock1.poll_lock().is_not_ready());
            assert!(lock2.poll_lock().is_not_ready());
            assert!(lock3.poll_lock().is_not_ready());
            drop(guard);

            assert!(lock1.poll_lock().is_ready());
            assert!(lock2.poll_lock().is_ready());
            assert!(lock3.poll_lock().is_ready());

            let guard = match lock2.poll_lock() {
                Async::Ready(g) => g,
                Async::NotReady => panic!("poll not ready"),
            };
            assert_eq!(*guard, 2);

            assert!(lock1.poll_lock().is_not_ready());
            assert!(lock2.poll_lock().is_not_ready());
            assert!(lock3.poll_lock().is_not_ready());

            Ok::<(), ()>(())
        });

        assert!(executor::spawn(future)
                .poll_future(unpark_noop())
                .expect("failure in poll")
                .is_ready());
    }

    #[test]
    fn concurrent() {
        const N: usize = 10000;
        let lock1 = FutMutex::new(0);
        let lock2 = lock1.clone();

        let a = Increment {
            a: Some(lock1),
            remaining: N,
        };
        let b = stream::iter((0..N).map(Ok::<_, ()>)).fold(lock2, |b, _n| {
            b.lock().map(|mut b| {
                *b += 1;
                b.unlock()
            })
        });

        let t1 = thread::spawn(move || a.wait());
        let b = b.wait().expect("b error");
        let a = t1.join().unwrap().expect("a error");

        match a.poll_lock() {
            Async::Ready(l) => assert_eq!(*l, 2 * N),
            Async::NotReady => panic!("poll not ready"),
        }
        match b.poll_lock() {
            Async::Ready(l) => assert_eq!(*l, 2 * N),
            Async::NotReady => panic!("poll not ready"),
        }

        struct Increment {
            remaining: usize,
            a: Option<FutMutex<usize>>,
        }

        impl Future for Increment {
            type Item = FutMutex<usize>;
            type Error = ();

            fn poll(&mut self) -> Poll<FutMutex<usize>, ()> {
                loop {
                    if self.remaining == 0 {
                        return Ok(self.a.take().unwrap().into())
                    }

                    let a = self.a.as_ref().unwrap();
                    let mut a = match a.poll_lock() {
                        Async::Ready(l) => l,
                        Async::NotReady => return Ok(Async::NotReady),
                    };
                    self.remaining -= 1;
                    *a += 1;
                }
            }
        }
    }
}
