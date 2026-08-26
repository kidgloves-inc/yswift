use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use yrs::{Origin, Subscription};

/// Ends an observation when dropped.
///
/// Two shapes. yrs's own [Subscription] only *queues* its removal since 0.27 —
/// the observer drops the callback on its next trigger — so a cancelled closure
/// would stay alive until the next event on that type. The shared types
/// therefore observe under a key of ours and unobserve by that key, which
/// removes the callback at once (`YTextTests.test_closure_observation_IsNotLeakingAfterUnobserving`
/// and its array/map twins pin this).
pub(crate) struct YSubscription {
    inner: Mutex<Option<Inner>>,
}

enum Inner {
    Deferred(#[allow(dead_code)] Subscription),
    Keyed(Box<dyn FnOnce() + Send>),
}

impl YSubscription {
    pub(crate) fn new(value: Subscription) -> YSubscription {
        YSubscription {
            inner: Mutex::new(Some(Inner::Deferred(value))),
        }
    }

    pub(crate) fn keyed<F>(unobserve: F) -> YSubscription
    where
        F: FnOnce() + Send + 'static,
    {
        YSubscription {
            inner: Mutex::new(Some(Inner::Keyed(Box::new(unobserve)))),
        }
    }
}

impl Drop for YSubscription {
    fn drop(&mut self) {
        let taken = self.inner.get_mut().map(Option::take).unwrap_or(None);
        match taken {
            Some(Inner::Keyed(unobserve)) => unobserve(),
            // Dropping the yrs Subscription queues its removal.
            Some(Inner::Deferred(_)) | None => {}
        }
    }
}

/// A process-unique observation key. Keys only need to be distinct within one
/// observer, so one counter for all of them is more than enough.
pub(crate) fn next_observation_key() -> Origin {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    Origin::from(NEXT.fetch_add(1, Ordering::Relaxed))
}
