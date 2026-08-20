//! A priority-ordered MPMC channel: one discrete crossbeam channel per
//! priority, indexed by a lock-free atomic bitset so a consumer can jump
//! straight to the lowest non-empty priority instead of polling all of them.
//!
//! # The one hard problem
//!
//! Everything here is one pattern applied at three heights: the **clear then
//! re-check** guard (a Dekker / store-buffer litmus). A summary/ready bit is
//! only ever a *hint* — "there might be work below me". False positives are
//! harmless (a consumer does one wasted probe and clears the bit); false
//! negatives — work present but its bit clear with nobody looking — are the
//! bug we must forbid. The invariant is preserved by always ordering:
//!
//! * **producer:** commit the work, *then* set the bit (store-store, safe under
//!   `Release`, but we use `SeqCst` throughout for one uniform mental model);
//! * **consumer:** clear the bit, *then* re-check the thing it summarised
//!   (store-load — this reordering is only forbidden under `SeqCst`).
//!
//! Labelling the four ops `A: commit`, `B: set`, `C: clear`, `D: re-check`, a
//! lost update needs `A < B < C < D < A`, a cycle, which a single total order
//! (SeqCst) makes impossible. The three heights the guard is applied at:
//!
//! 1. summary-bit ↔ leaf word — inside [`AtomicBitset4096::clear`] / `find_lowest`;
//! 2. leaf-bit ↔ channel contents — inside [`Consumer::try_recv`];
//! 3. sleeping-consumer ↔ ready-set — inside [`EventCount`].

use crossbeam_channel::{Receiver, RecvError, SendError, Sender, TryRecvError};
use std::array;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Number of priorities / channels. Two-level bitset: 64 leaves × 64 bits.
pub const PRIORITIES: usize = 64 * 64;

/// Backstop wake for a sleeping consumer. Correctness does not depend on it —
/// [`EventCount`] wakes precisely on new work and on shutdown — it only bounds
/// the damage of any unforeseen missed wake. Idle cost is one no-op wake per
/// consumer per interval.
const SLEEP_BACKSTOP: Duration = Duration::from_millis(250);

/// A lock-free "which priorities have work" index for [`PRIORITIES`] bits.
///
/// `summary` bit `hi` is a hint that `leaves[hi]` *might* be non-zero; each set
/// leaf bit is an exact ready flag for one priority. See the module docs for
/// why the hint can over- but never under-report.
pub struct AtomicBitset4096 {
    summary: AtomicU64,
    leaves: [AtomicU64; 64],
}

impl AtomicBitset4096 {
    pub fn new() -> Self {
        Self {
            summary: AtomicU64::new(0),
            leaves: array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Mark priority `p` ready. Sets the leaf bit *before* advertising upward,
    /// so the summary never points at a leaf that isn't yet set.
    #[inline]
    pub fn set(&self, p: usize) {
        debug_assert!(p < PRIORITIES);
        let (hi, lo) = (p >> 6, p & 63);
        self.leaves[hi].fetch_or(1u64 << lo, SeqCst);
        self.summary.fetch_or(1u64 << hi, SeqCst);
    }

    /// Clear priority `p`. When that empties the leaf, retract the summary bit —
    /// then re-check the leaf, because a concurrent [`set`](Self::set) may have
    /// refilled it in the store-load window (the level-1 Dekker guard).
    #[inline]
    pub fn clear(&self, p: usize) {
        debug_assert!(p < PRIORITIES);
        let (hi, lo) = (p >> 6, p & 63);
        let after = self.leaves[hi].fetch_and(!(1u64 << lo), SeqCst) & !(1u64 << lo);
        if after == 0 {
            self.summary.fetch_and(!(1u64 << hi), SeqCst); // C: store
            if self.leaves[hi].load(SeqCst) != 0 {
                // D: load saw a refill — re-advertise so it isn't stranded.
                self.summary.fetch_or(1u64 << hi, SeqCst);
            }
        }
    }

    /// Lowest ready priority, or `None`. Self-heals stale summary bits with the
    /// same clear-then-recheck guard rather than trusting the hint blindly.
    #[inline]
    pub fn find_lowest(&self) -> Option<usize> {
        loop {
            let s = self.summary.load(SeqCst);
            if s == 0 {
                return None;
            }
            let hi = s.trailing_zeros() as usize;
            let leaf = self.leaves[hi].load(SeqCst);
            if leaf == 0 {
                // Stale summary bit: clear it, then re-check for a refill.
                self.summary.fetch_and(!(1u64 << hi), SeqCst);
                if self.leaves[hi].load(SeqCst) != 0 {
                    self.summary.fetch_or(1u64 << hi, SeqCst);
                }
                continue;
            }
            return Some((hi << 6) | leaf.trailing_zeros() as usize);
        }
    }

    /// Fast "is anything ready?" — reads only the summary word.
    #[inline]
    pub fn any(&self) -> bool {
        self.summary.load(SeqCst) != 0
    }
}

impl Default for AtomicBitset4096 {
    fn default() -> Self {
        Self::new()
    }
}

/// The MPMC blocking primitive: an *eventcount*. Lets N consumers sleep until a
/// producer publishes work, with no lost wakeups and no lock on the send hot
/// path while consumers are busy.
///
/// The `waiters` counter and the ready-set summary form the level-3 Dekker pair:
/// a consumer does `waiters++` then re-checks the predicate; a producer sets the
/// ready bit then reads `waiters`. Under `SeqCst` they cannot both miss each
/// other, so a consumer never sleeps through available work.
struct EventCount {
    lock: Mutex<()>,
    cond: Condvar,
    waiters: AtomicUsize,
}

impl EventCount {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            cond: Condvar::new(),
            waiters: AtomicUsize::new(0),
        }
    }

    /// Sleep until `has_work()` may be true. Registers as a waiter first, then
    /// re-checks the predicate both before and (under the lock) after — closing
    /// the register/publish and the check/notify windows respectively.
    fn wait_if_empty(&self, has_work: impl Fn() -> bool) {
        self.waiters.fetch_add(1, SeqCst);
        if has_work() {
            // Work appeared between the caller's probe and our registration.
            self.waiters.fetch_sub(1, SeqCst);
            return;
        }
        {
            let guard = self.lock.lock().unwrap();
            // Re-check under the lock: a notify() can only fire while holding it,
            // so if work exists now we won't miss the wake by sleeping.
            if !has_work() {
                let _ = self.cond.wait_timeout(guard, SLEEP_BACKSTOP).unwrap();
            }
        }
        self.waiters.fetch_sub(1, SeqCst);
    }

    /// Wake sleepers after publishing work. Skips the lock entirely when there
    /// are no waiters (the common case while consumers are draining) — the
    /// `SeqCst` load pairs with each waiter's post-registration re-check.
    fn notify(&self) {
        if self.waiters.load(SeqCst) == 0 {
            return;
        }
        let _g = self.lock.lock().unwrap();
        self.cond.notify_all();
    }

    /// Unconditional wake, used on shutdown where we must rouse every sleeper
    /// even if `waiters` momentarily reads zero mid-registration.
    fn notify_all(&self) {
        let _g = self.lock.lock().unwrap();
        self.cond.notify_all();
    }
}

/// Shared guts held by every [`Producer`] and [`Consumer`] via `Arc`.
struct Inner<T> {
    ready: AtomicBitset4096,
    tx: Box<[Sender<T>]>,
    rx: Box<[Receiver<T>]>,
    wait: EventCount,
    /// Live [`Producer`] handles. Reaching 0 means no further sends can occur,
    /// which is how a blocking [`Consumer::recv`] learns to return disconnected.
    live_producers: AtomicUsize,
    /// Live [`Consumer`] handles. Reaching 0 lets [`Producer::send`] fail fast
    /// instead of piling messages into channels nobody will ever drain.
    live_consumers: AtomicUsize,
}

/// Create a priority channel, returning one producer and one consumer handle.
/// Both are cloneable; clones share the same underlying channels.
pub fn channel<T>() -> (Producer<T>, Consumer<T>) {
    let mut tx = Vec::with_capacity(PRIORITIES);
    let mut rx = Vec::with_capacity(PRIORITIES);
    for _ in 0..PRIORITIES {
        let (s, r) = crossbeam_channel::unbounded();
        tx.push(s);
        rx.push(r);
    }
    let inner = Arc::new(Inner {
        ready: AtomicBitset4096::new(),
        tx: tx.into_boxed_slice(),
        rx: rx.into_boxed_slice(),
        wait: EventCount::new(),
        live_producers: AtomicUsize::new(1),
        live_consumers: AtomicUsize::new(1),
    });
    (
        Producer {
            inner: inner.clone(),
        },
        Consumer { inner },
    )
}

/// Sending half. `Clone` to fan in from multiple producer threads.
pub struct Producer<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Producer<T> {
    /// Enqueue `msg` at `priority` (`0` = highest). Fails only when every
    /// consumer has been dropped.
    pub fn send(&self, priority: usize, msg: T) -> Result<(), SendError<T>> {
        assert!(priority < PRIORITIES, "priority {priority} out of range");
        if self.inner.live_consumers.load(SeqCst) == 0 {
            return Err(SendError(msg));
        }
        // Commit → advertise → wake. Each step must not precede the last:
        // never advertise an uncommitted message, never wake before advertising.
        self.inner.tx[priority].send(msg)?;
        self.inner.ready.set(priority);
        self.inner.wait.notify();
        Ok(())
    }

    /// Number of priorities. Convenience for callers bucketing into the range.
    pub const fn priorities(&self) -> usize {
        PRIORITIES
    }
}

impl<T> Clone for Producer<T> {
    fn clone(&self) -> Self {
        self.inner.live_producers.fetch_add(1, SeqCst);
        Producer {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Producer<T> {
    fn drop(&mut self) {
        if self.inner.live_producers.fetch_sub(1, SeqCst) == 1 {
            // Last producer gone: rouse every sleeper so blocking recv()s can
            // observe the disconnect and return.
            self.inner.wait.notify_all();
        }
    }
}

/// Receiving half. `Clone` to fan out to multiple consumer threads.
pub struct Consumer<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Consumer<T> {
    /// Pop the highest-priority available message without blocking.
    ///
    /// Returns [`TryRecvError::Disconnected`] only once every producer is gone
    /// *and* the ready-set is empty (re-checked, so a message published just
    /// before the last producer dropped is never reported as disconnected).
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let inner = &*self.inner;
        loop {
            let Some(p) = inner.ready.find_lowest() else {
                if inner.live_producers.load(SeqCst) == 0 && inner.ready.find_lowest().is_none() {
                    return Err(TryRecvError::Disconnected);
                }
                return Err(TryRecvError::Empty);
            };
            match inner.rx[p].try_recv() {
                // Leave the leaf bit set: there may be more behind this one.
                Ok(msg) => return Ok(msg),
                Err(TryRecvError::Empty) => {
                    // Level-2 Dekker: provisionally clear, then re-check. Another
                    // consumer may have drained it, or a producer refilled it.
                    inner.ready.clear(p);
                    match inner.rx[p].try_recv() {
                        Ok(msg) => {
                            // A send slipped in; re-advertise before returning.
                            inner.ready.set(p);
                            return Ok(msg);
                        }
                        // Genuinely empty now (bit stays clear); scan again.
                        Err(_) => continue,
                    }
                }
                // A per-channel disconnect can't actually happen (both ends live
                // in Inner) but handle it defensively: drop the stale bit.
                Err(TryRecvError::Disconnected) => {
                    inner.ready.clear(p);
                    continue;
                }
            }
        }
    }

    /// Pop the highest-priority message, blocking until one is available or the
    /// channel disconnects (all producers dropped and nothing left to drain).
    pub fn recv(&self) -> Result<T, RecvError> {
        let inner = &*self.inner;
        loop {
            match self.try_recv() {
                Ok(msg) => return Ok(msg),
                Err(TryRecvError::Disconnected) => return Err(RecvError),
                Err(TryRecvError::Empty) => {
                    // Sleep until work is published or the last producer leaves.
                    inner.wait.wait_if_empty(|| {
                        inner.ready.any() || inner.live_producers.load(SeqCst) == 0
                    });
                }
            }
        }
    }
}

impl<T> Clone for Consumer<T> {
    fn clone(&self) -> Self {
        self.inner.live_consumers.fetch_add(1, SeqCst);
        Consumer {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Drop for Consumer<T> {
    fn drop(&mut self) {
        self.inner.live_consumers.fetch_sub(1, SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;
    use std::thread;

    // Deterministic xorshift so tests don't pull in `rand`.
    fn next(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn bitset_set_find_clear() {
        let b = AtomicBitset4096::new();
        assert_eq!(b.find_lowest(), None);
        for &p in &[63, 64, 4095, 0, 130] {
            b.set(p);
        }
        assert_eq!(b.find_lowest(), Some(0));
        b.clear(0);
        assert_eq!(b.find_lowest(), Some(63));
        b.clear(63);
        assert_eq!(b.find_lowest(), Some(64)); // crosses leaf boundary
        b.clear(64);
        b.clear(130);
        assert_eq!(b.find_lowest(), Some(4095));
        b.clear(4095);
        assert_eq!(b.find_lowest(), None);
        assert!(!b.any());
    }

    #[test]
    fn strict_priority_order_single_consumer() {
        let (tx, rx) = channel::<usize>();
        let input = [5usize, 2, 9, 2, 0, 63, 64, 4095, 1];
        for &p in &input {
            tx.send(p, p).unwrap();
        }
        let mut got = Vec::new();
        while let Ok(p) = rx.try_recv() {
            got.push(p);
        }
        let mut want = input.to_vec();
        want.sort_unstable();
        assert_eq!(got, want, "must dequeue in non-decreasing priority order");
    }

    #[test]
    fn mpmc_no_message_loss() {
        const PRODUCERS: usize = 4;
        const CONSUMERS: usize = 4;
        const PER_PRODUCER: usize = 5_000;

        let (tx, rx) = channel::<usize>();
        let sent: Arc<Vec<AtomicUsize>> =
            Arc::new((0..PRIORITIES).map(|_| AtomicUsize::new(0)).collect());
        let recvd: Arc<Vec<AtomicUsize>> =
            Arc::new((0..PRIORITIES).map(|_| AtomicUsize::new(0)).collect());

        let consumers: Vec<_> = (0..CONSUMERS)
            .map(|_| {
                let rx = rx.clone();
                let recvd = recvd.clone();
                thread::spawn(move || {
                    while let Ok(p) = rx.recv() {
                        recvd[p].fetch_add(1, Relaxed);
                    }
                })
            })
            .collect();
        drop(rx); // only the CONSUMERS clones keep the receiving side alive

        let producers: Vec<_> = (0..PRODUCERS)
            .map(|t| {
                let tx = tx.clone();
                let sent = sent.clone();
                thread::spawn(move || {
                    let mut state = t as u64 + 1;
                    for _ in 0..PER_PRODUCER {
                        let p = (next(&mut state) as usize) % PRIORITIES;
                        sent[p].fetch_add(1, Relaxed);
                        tx.send(p, p).unwrap();
                    }
                })
            })
            .collect();
        drop(tx); // once the producer threads finish, live_producers hits 0

        for h in producers {
            h.join().unwrap();
        }
        for h in consumers {
            h.join().unwrap();
        }

        let total: usize = recvd.iter().map(|c| c.load(Relaxed)).sum();
        assert_eq!(total, PRODUCERS * PER_PRODUCER, "message count mismatch");
        for p in 0..PRIORITIES {
            assert_eq!(
                sent[p].load(Relaxed),
                recvd[p].load(Relaxed),
                "per-priority mismatch at {p}"
            );
        }
    }

    #[test]
    fn blocking_recv_wakes_on_send() {
        let (tx, rx) = channel::<u32>();
        let h = thread::spawn(move || rx.recv().unwrap());
        thread::sleep(Duration::from_millis(50)); // ensure the consumer is parked
        tx.send(10, 42).unwrap();
        assert_eq!(h.join().unwrap(), 42);
    }

    #[test]
    fn blocked_consumers_wake_on_disconnect() {
        let (tx, rx) = channel::<u32>();
        let consumers: Vec<_> = (0..4)
            .map(|_| {
                let rx = rx.clone();
                thread::spawn(move || rx.recv().is_err()) // expect Err(Disconnected)
            })
            .collect();
        drop(rx);
        thread::sleep(Duration::from_millis(50)); // let them all park
        drop(tx); // triggers notify_all()
        for h in consumers {
            assert!(h.join().unwrap(), "consumer should observe disconnect");
        }
    }

    #[test]
    fn send_fails_when_no_consumers() {
        let (tx, rx) = channel::<u32>();
        drop(rx);
        assert!(tx.send(0, 1).is_err());
    }
}
