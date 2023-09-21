use crate::mutex::MutexGuard;
use crate::raw_mutex::RawMutex;
use event::Event;
use lock_api::RawMutex as LockApiRawMutex;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::time::{Duration, Instant};

/// A type indicating whether a timed wait on a condition variable returned
/// due to a time out or not.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns whether the wait was known to have timed out.
    #[inline]
    pub fn timed_out(self) -> bool {
        self.0
    }
}

#[derive(Debug)]
pub struct Condvar {
    wait_queue: Event,
    current_mutex: AtomicPtr<RawMutex>,
}

impl Condvar {
    pub const fn new() -> Self {
        Self {
            wait_queue: Event::new(),
            current_mutex: AtomicPtr::new(null_mut()),
        }
    }

    pub fn notify_one(&self) -> bool {
        self.wait_queue.notify_if(
            |num_left| {
                if num_left == 1 {
                    self.current_mutex.store(null_mut(), Ordering::Relaxed);
                }
                true
            },
            || {
                self.current_mutex.store(null_mut(), Ordering::Relaxed);
            },
        )
    }

    pub fn notify_all(&self) -> usize {
        self.wait_queue.notify_all_while(
            |num_left| {
                if num_left == 1 {
                    self.current_mutex.store(null_mut(), Ordering::Relaxed);
                }
                true
            },
            || {
                self.current_mutex.store(null_mut(), Ordering::Relaxed);
            },
        )
    }

    pub fn notify_many(&self, max: usize) -> usize {
        self.wait_queue.notify_many_while(
            max,
            |num_left| {
                if num_left == 1 {
                    self.current_mutex.store(null_mut(), Ordering::Relaxed);
                }
                true
            },
            || {
                self.current_mutex.store(null_mut(), Ordering::Relaxed);
            },
        )
    }

    const fn will_sleep<'a>(&'a self, mutex: &'a RawMutex) -> impl Fn(usize) -> bool + 'a {
        move |_| {
            let current_mutex = self
                .current_mutex
                .swap(mutex as *const _ as *mut _, Ordering::Relaxed)
                .cast_const();
            assert!(
                current_mutex.is_null() || current_mutex == mutex,
                "Condvar wait called with two different mutexes"
            );
            debug_assert!(mutex.is_locked());
            unsafe {
                mutex.unlock();
            }
            true
        }
    }

    pub fn wait<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>) {
        let mutex = unsafe { MutexGuard::mutex(guard).raw() };
        debug_assert!(mutex.is_locked());
        self.wait_queue.wait_once(self.will_sleep(mutex));
        mutex.lock();
    }

    pub fn wait_until<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: Instant,
    ) -> WaitTimeoutResult {
        let mutex = unsafe { MutexGuard::mutex(guard).raw() };
        debug_assert!(mutex.is_locked());
        let result = !self
            .wait_queue
            .wait_while_until(self.will_sleep(mutex), || true, timeout);
        mutex.lock();
        WaitTimeoutResult(result)
    }

    pub fn wait_for<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        timeout: Duration,
    ) -> WaitTimeoutResult {
        if let Some(timeout) = Instant::now().checked_add(timeout) {
            return self.wait_until(guard, timeout);
        }
        self.wait(guard);
        WaitTimeoutResult(false)
    }

    pub fn wait_while<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        mut condition: impl FnMut(&mut T) -> bool,
    ) {
        let mutex = unsafe { MutexGuard::mutex(guard).raw() };
        while condition(&mut *guard) {
            debug_assert!(mutex.is_locked());
            self.wait_queue.wait_once(self.will_sleep(mutex));
            mutex.lock();
        }
    }

    pub fn wait_while_until<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        mut condition: impl FnMut(&mut T) -> bool,
        timeout: Instant,
    ) -> WaitTimeoutResult {
        let mutex = unsafe { MutexGuard::mutex(guard).raw() };
        while condition(&mut *guard) {
            debug_assert!(mutex.is_locked());
            let result =
                !self
                    .wait_queue
                    .wait_while_until(self.will_sleep(mutex), || true, timeout);
            mutex.lock();
            if result {
                return WaitTimeoutResult(true);
            }
        }
        WaitTimeoutResult(false)
    }

    pub fn wait_while_for<T: ?Sized>(
        &self,
        guard: &mut MutexGuard<'_, T>,
        condition: impl FnMut(&mut T) -> bool,
        timeout: Duration,
    ) -> WaitTimeoutResult {
        let timeout = Instant::now() + timeout;
        self.wait_while_until(guard, condition, timeout)
    }

    pub async fn async_wait<T: ?Sized>(&self, guard: &mut MutexGuard<'_, T>) {
        let mutex = unsafe { MutexGuard::mutex(guard).raw() };
        debug_assert!(mutex.is_locked());
        self.wait_queue
            .wait_while_async(self.will_sleep(mutex), || true)
            .await;
        mutex.lock_async().await;
    }
}

#[cfg(test)]
mod my_tests {
    use super::*;
    use crate::mutex::Mutex;
    use std::sync::Arc;

    #[test]
    fn test_condvar() {
        static CONDVAR: Condvar = Condvar::new();
        let mutex = Arc::new(Mutex::new(()));
        let mutex2 = mutex.clone();
        let mut guard = mutex.lock();
        let handle = std::thread::spawn(move || {
            let _guard = mutex2.lock();
            CONDVAR.notify_one();
        });
        CONDVAR.wait(&mut guard);
        handle.join().unwrap();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn async_test_condvar() {
        static CONDVAR: Condvar = Condvar::new();
        let mutex = Arc::new(Mutex::new(()));
        let mutex2 = mutex.clone();
        let mut guard = mutex.lock();
        let handle = tokio::spawn(async move {
            let _guard = mutex2.lock_async().await;
            CONDVAR.notify_one();
        });
        CONDVAR.async_wait(&mut guard).await;
        handle.await.unwrap();
    }
}

#[cfg(test)]
mod tests {
    use crate::{condvar::Condvar, mutex::Mutex, mutex::MutexGuard};
    use std::sync::mpsc::channel;
    use std::sync::Arc;
    use std::thread;
    use std::thread::sleep;
    use std::thread::JoinHandle;
    use std::time::Duration;
    use std::time::Instant;

    #[test]
    fn smoke() {
        let c = Condvar::new();
        c.notify_one();
        c.notify_all();
    }

    #[test]
    fn notify_one() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let mut g = m.lock();
        let _t = thread::spawn(move || {
            let _g = m2.lock();
            c2.notify_one();
        });
        c.wait(&mut g);
    }

    #[test]
    fn notify_all() {
        const N: usize = 10;

        let data = Arc::new((Mutex::new(0), Condvar::new()));
        let (tx, rx) = channel();
        for _ in 0..N {
            let data = data.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                let &(ref lock, ref cond) = &*data;
                let mut cnt = lock.lock();
                *cnt += 1;
                if *cnt == N {
                    tx.send(()).unwrap();
                }
                while *cnt != 0 {
                    cond.wait(&mut cnt);
                }
                tx.send(()).unwrap();
            });
        }
        drop(tx);

        let &(ref lock, ref cond) = &*data;
        rx.recv().unwrap();
        let mut cnt = lock.lock();
        *cnt = 0;
        cond.notify_all();
        drop(cnt);

        for _ in 0..N {
            rx.recv().unwrap();
        }
    }

    #[test]
    fn notify_one_return_true() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let mut g = m.lock();
        let _t = thread::spawn(move || {
            let _g = m2.lock();
            assert!(c2.notify_one());
        });
        c.wait(&mut g);
    }

    #[test]
    fn notify_one_return_false() {
        let m = Arc::new(Mutex::new(()));
        let c = Arc::new(Condvar::new());

        let _t = thread::spawn(move || {
            let _g = m.lock();
            assert!(!c.notify_one());
        });
    }

    #[test]
    fn notify_all_return() {
        const N: usize = 10;

        let data = Arc::new((Mutex::new(0), Condvar::new()));
        let (tx, rx) = channel();
        for _ in 0..N {
            let data = data.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                let &(ref lock, ref cond) = &*data;
                let mut cnt = lock.lock();
                *cnt += 1;
                if *cnt == N {
                    tx.send(()).unwrap();
                }
                while *cnt != 0 {
                    cond.wait(&mut cnt);
                }
                tx.send(()).unwrap();
            });
        }
        drop(tx);

        let &(ref lock, ref cond) = &*data;
        rx.recv().unwrap();
        let mut cnt = lock.lock();
        *cnt = 0;
        assert_eq!(cond.notify_all(), N);
        drop(cnt);

        for _ in 0..N {
            rx.recv().unwrap();
        }

        assert_eq!(cond.notify_all(), 0);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    //TODO miri doesn't like this. Is there a bug or is it just miri?
    fn wait_for() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let mut g = m.lock();
        let no_timeout = c.wait_for(&mut g, Duration::from_millis(1));
        assert!(no_timeout.timed_out());

        let _t = thread::spawn(move || {
            let _g = m2.lock();
            c2.notify_one();
        });
        let timeout_res = c.wait_for(&mut g, Duration::from_secs(u64::max_value()));
        assert!(!timeout_res.timed_out());

        drop(g);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    //TODO miri doesn't like this. Is there a bug or is it just miri?
    fn wait_until() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let mut g = m.lock();
        let no_timeout = c.wait_until(&mut g, Instant::now() + Duration::from_millis(1));
        assert!(no_timeout.timed_out());
        let _t = thread::spawn(move || {
            let _g = m2.lock();
            c2.notify_one();
        });
        let timeout_res = c.wait_until(
            &mut g,
            Instant::now() + Duration::from_millis(u32::max_value() as u64),
        );
        assert!(!timeout_res.timed_out());
        drop(g);
    }

    fn spawn_wait_while_notifier(
        mutex: Arc<Mutex<u32>>,
        cv: Arc<Condvar>,
        num_iters: u32,
        timeout: Option<Instant>,
    ) -> JoinHandle<()> {
        thread::spawn(move || {
            for epoch in 1..=num_iters {
                // spin to wait for main test thread to block
                // before notifying it to wake back up and check
                // its condition.
                let mut sleep_backoff = Duration::from_millis(1);
                let _mutex_guard = loop {
                    let mutex_guard = mutex.lock();

                    if let Some(timeout) = timeout {
                        if Instant::now() >= timeout {
                            return;
                        }
                    }

                    if *mutex_guard == epoch {
                        break mutex_guard;
                    }

                    drop(mutex_guard);

                    // give main test thread a good chance to
                    // acquire the lock before this thread does.
                    sleep(sleep_backoff);
                    sleep_backoff *= 2;
                };

                cv.notify_one();
            }
        })
    }

    #[test]
    fn wait_while_until_internal_does_not_wait_if_initially_false() {
        let mutex = Arc::new(Mutex::new(0));
        let cv = Arc::new(Condvar::new());

        let condition = |counter: &mut u32| {
            *counter += 1;
            false
        };

        let mut mutex_guard = mutex.lock();
        cv.wait_while(&mut mutex_guard, condition);

        assert!(*mutex_guard == 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    //TODO miri doesn't like this. Is there a bug or is it just miri?
    fn wait_while_until_internal_times_out_before_false() {
        let mutex = Arc::new(Mutex::new(0));
        let cv = Arc::new(Condvar::new());

        let num_iters = 3;
        let condition = |counter: &mut u32| {
            *counter += 1;
            true
        };

        let mut mutex_guard = mutex.lock();
        let timeout = Instant::now() + Duration::from_millis(500);
        let handle = spawn_wait_while_notifier(mutex.clone(), cv.clone(), num_iters, Some(timeout));

        let timeout_result = cv.wait_while_until(&mut mutex_guard, condition, timeout);

        assert!(timeout_result.timed_out());
        assert!(*mutex_guard == num_iters + 1);

        // prevent deadlock with notifier
        drop(mutex_guard);
        handle.join().unwrap();
    }

    #[test]
    fn wait_while_until_internal() {
        let mutex = Arc::new(Mutex::new(0));
        let cv = Arc::new(Condvar::new());

        let num_iters = 4;

        let condition = |counter: &mut u32| {
            *counter += 1;
            *counter <= num_iters
        };

        let mut mutex_guard = mutex.lock();
        let handle = spawn_wait_while_notifier(mutex.clone(), cv.clone(), num_iters, None);

        cv.wait_while(&mut mutex_guard, condition);

        assert!(*mutex_guard == num_iters + 1);

        cv.wait_while(&mut mutex_guard, condition);
        handle.join().unwrap();

        assert!(*mutex_guard == num_iters + 2);
    }

    #[test]
    #[should_panic]
    fn two_mutexes() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let m3 = Arc::new(Mutex::new(()));
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        // Make sure we don't leave the child thread dangling
        struct PanicGuard<'a>(&'a Condvar);
        impl<'a> Drop for PanicGuard<'a> {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }

        let (tx, rx) = channel();
        let g = m.lock();
        let _t = thread::spawn(move || {
            let mut g = m2.lock();
            tx.send(()).unwrap();
            c2.wait(&mut g);
        });
        drop(g);
        rx.recv().unwrap();
        let _g = m.lock();
        let _guard = PanicGuard(&*c);
        c.wait(&mut m3.lock());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    //TODO this fails under miri should it..?
    fn two_mutexes_disjoint() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let m3 = Arc::new(Mutex::new(()));
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();

        let mut g = m.lock();
        let _t = thread::spawn(move || {
            let _g = m2.lock();
            c2.notify_one();
        });
        c.wait(&mut g);
        drop(g);

        let _ = c.wait_for(&mut m3.lock(), Duration::from_millis(1));
    }

    #[test]
    fn test_condvar_requeue() {
        let m = Arc::new(Mutex::new(()));
        let m2 = m.clone();
        let c = Arc::new(Condvar::new());
        let c2 = c.clone();
        let t = thread::spawn(move || {
            let mut g = m2.lock();
            c2.wait(&mut g);
        });

        let mut g = m.lock();
        while !c.notify_one() {
            // Wait for the thread to get into wait()
            MutexGuard::bump(&mut g);
            // Yield, so the other thread gets a chance to do something.
            // (At least Miri needs this, because it doesn't preempt threads.)
            thread::yield_now();
        }
        // The thread should have been requeued to the mutex, which we wake up now.
        drop(g);
        t.join().unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_issue_129() {
        let locks = Arc::new((Mutex::new(()), Condvar::new()));

        let (tx, rx) = channel();
        for _ in 0..4 {
            let locks = locks.clone();
            let tx = tx.clone();
            thread::spawn(move || {
                let mut guard = locks.0.lock();
                locks.1.wait(&mut guard);
                locks.1.wait_for(&mut guard, Duration::from_millis(1));
                locks.1.notify_one();
                tx.send(()).unwrap();
            });
        }

        thread::sleep(Duration::from_millis(100));
        locks.1.notify_one();

        for _ in 0..4 {
            assert_eq!(rx.recv_timeout(Duration::from_millis(500)), Ok(()));
        }
    }
}

/// This module contains an integration test that is heavily inspired from WebKit's own integration
/// tests for it's own Condvar.
#[cfg(test)]
mod webkit_queue_test {
    use crate::{condvar::Condvar, mutex::Mutex, mutex::MutexGuard};
    use std::{collections::VecDeque, sync::Arc, thread, time::Duration};

    #[derive(Clone, Copy)]
    enum Timeout {
        Bounded(Duration),
        Forever,
    }

    #[derive(Clone, Copy)]
    enum NotifyStyle {
        One,
        All,
    }

    struct Queue {
        items: VecDeque<usize>,
        should_continue: bool,
    }

    impl Queue {
        fn new() -> Self {
            Self {
                items: VecDeque::new(),
                should_continue: true,
            }
        }
    }

    fn wait<T: ?Sized>(
        condition: &Condvar,
        lock: &mut MutexGuard<'_, T>,
        predicate: impl Fn(&mut MutexGuard<'_, T>) -> bool,
        timeout: &Timeout,
    ) {
        while !predicate(lock) {
            match timeout {
                Timeout::Forever => condition.wait(lock),
                Timeout::Bounded(bound) => {
                    condition.wait_for(lock, *bound);
                }
            }
        }
    }

    fn notify(style: NotifyStyle, condition: &Condvar, should_notify: bool) {
        match style {
            NotifyStyle::One => {
                condition.notify_one();
            }
            NotifyStyle::All => {
                if should_notify {
                    condition.notify_all();
                }
            }
        }
    }

    fn run_queue_test(
        num_producers: usize,
        num_consumers: usize,
        max_queue_size: usize,
        messages_per_producer: usize,
        notify_style: NotifyStyle,
        timeout: Timeout,
        delay: Duration,
    ) {
        let input_queue = Arc::new(Mutex::new(Queue::new()));
        let empty_condition = Arc::new(Condvar::new());
        let full_condition = Arc::new(Condvar::new());

        let output_vec = Arc::new(Mutex::new(vec![]));

        let consumers = (0..num_consumers)
            .map(|_| {
                consumer_thread(
                    input_queue.clone(),
                    empty_condition.clone(),
                    full_condition.clone(),
                    timeout,
                    notify_style,
                    output_vec.clone(),
                    max_queue_size,
                )
            })
            .collect::<Vec<_>>();
        let producers = (0..num_producers)
            .map(|_| {
                producer_thread(
                    messages_per_producer,
                    input_queue.clone(),
                    empty_condition.clone(),
                    full_condition.clone(),
                    timeout,
                    notify_style,
                    max_queue_size,
                )
            })
            .collect::<Vec<_>>();

        thread::sleep(delay);

        for producer in producers.into_iter() {
            producer.join().expect("Producer thread panicked");
        }

        {
            let mut input_queue = input_queue.lock();
            input_queue.should_continue = false;
        }
        empty_condition.notify_all();

        for consumer in consumers.into_iter() {
            consumer.join().expect("Consumer thread panicked");
        }

        let mut output_vec = output_vec.lock();
        assert_eq!(output_vec.len(), num_producers * messages_per_producer);
        output_vec.sort();
        for msg_idx in 0..messages_per_producer {
            for producer_idx in 0..num_producers {
                assert_eq!(msg_idx, output_vec[msg_idx * num_producers + producer_idx]);
            }
        }
    }

    fn consumer_thread(
        input_queue: Arc<Mutex<Queue>>,
        empty_condition: Arc<Condvar>,
        full_condition: Arc<Condvar>,
        timeout: Timeout,
        notify_style: NotifyStyle,
        output_queue: Arc<Mutex<Vec<usize>>>,
        max_queue_size: usize,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || loop {
            let (should_notify, result) = {
                let mut queue = input_queue.lock();
                wait(
                    &*empty_condition,
                    &mut queue,
                    |state| -> bool { !state.items.is_empty() || !state.should_continue },
                    &timeout,
                );
                if queue.items.is_empty() && !queue.should_continue {
                    return;
                }
                let should_notify = queue.items.len() == max_queue_size;
                let result = queue.items.pop_front();
                std::mem::drop(queue);
                (should_notify, result)
            };
            notify(notify_style, &*full_condition, should_notify);

            if let Some(result) = result {
                output_queue.lock().push(result);
            }
        })
    }

    fn producer_thread(
        num_messages: usize,
        queue: Arc<Mutex<Queue>>,
        empty_condition: Arc<Condvar>,
        full_condition: Arc<Condvar>,
        timeout: Timeout,
        notify_style: NotifyStyle,
        max_queue_size: usize,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for message in 0..num_messages {
                let should_notify = {
                    let mut queue = queue.lock();
                    wait(
                        &*full_condition,
                        &mut queue,
                        |state| state.items.len() < max_queue_size,
                        &timeout,
                    );
                    let should_notify = queue.items.is_empty();
                    queue.items.push_back(message);
                    std::mem::drop(queue);
                    should_notify
                };
                notify(notify_style, &*empty_condition, should_notify);
            }
        })
    }

    macro_rules! run_queue_tests {
        ( $( $name:ident(
            num_producers: $num_producers:expr,
            num_consumers: $num_consumers:expr,
            max_queue_size: $max_queue_size:expr,
            messages_per_producer: $messages_per_producer:expr,
            notification_style: $notification_style:expr,
            timeout: $timeout:expr,
            delay_seconds: $delay_seconds:expr);
        )* ) => {
            $(#[test]
            #[cfg_attr(miri, ignore)]
            fn $name() {
                let delay = Duration::from_secs($delay_seconds);
                run_queue_test(
                    $num_producers,
                    $num_consumers,
                    $max_queue_size,
                    $messages_per_producer,
                    $notification_style,
                    $timeout,
                    delay,
                    );
            })*
        };
    }

    run_queue_tests! {
        sanity_check_queue(
            num_producers: 1,
            num_consumers: 1,
            max_queue_size: 1,
            messages_per_producer: 100_000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Bounded(Duration::from_secs(1)),
            delay_seconds: 0
        );
        sanity_check_queue_timeout(
            num_producers: 1,
            num_consumers: 1,
            max_queue_size: 1,
            messages_per_producer: 100_000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        new_test_without_timeout_5(
            num_producers: 1,
            num_consumers: 5,
            max_queue_size: 1,
            messages_per_producer: 100_000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        one_producer_one_consumer_one_slot(
            num_producers: 1,
            num_consumers: 1,
            max_queue_size: 1,
            messages_per_producer: 100_000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        one_producer_one_consumer_one_slot_timeout(
            num_producers: 1,
            num_consumers: 1,
            max_queue_size: 1,
            messages_per_producer: 100_000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 1
        );
        one_producer_one_consumer_hundred_slots(
            num_producers: 1,
            num_consumers: 1,
            max_queue_size: 100,
            messages_per_producer: 1_000_000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        ten_producers_one_consumer_one_slot(
            num_producers: 10,
            num_consumers: 1,
            max_queue_size: 1,
            messages_per_producer: 10000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        ten_producers_one_consumer_hundred_slots_notify_all(
            num_producers: 10,
            num_consumers: 1,
            max_queue_size: 100,
            messages_per_producer: 10000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        ten_producers_one_consumer_hundred_slots_notify_one(
            num_producers: 10,
            num_consumers: 1,
            max_queue_size: 100,
            messages_per_producer: 10000,
            notification_style: NotifyStyle::One,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        one_producer_ten_consumers_one_slot(
            num_producers: 1,
            num_consumers: 10,
            max_queue_size: 1,
            messages_per_producer: 10000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        one_producer_ten_consumers_hundred_slots_notify_all(
            num_producers: 1,
            num_consumers: 10,
            max_queue_size: 100,
            messages_per_producer: 100_000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        one_producer_ten_consumers_hundred_slots_notify_one(
            num_producers: 1,
            num_consumers: 10,
            max_queue_size: 100,
            messages_per_producer: 100_000,
            notification_style: NotifyStyle::One,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        ten_producers_ten_consumers_one_slot(
            num_producers: 10,
            num_consumers: 10,
            max_queue_size: 1,
            messages_per_producer: 50000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        ten_producers_ten_consumers_hundred_slots_notify_all(
            num_producers: 10,
            num_consumers: 10,
            max_queue_size: 100,
            messages_per_producer: 50000,
            notification_style: NotifyStyle::All,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
        ten_producers_ten_consumers_hundred_slots_notify_one(
            num_producers: 10,
            num_consumers: 10,
            max_queue_size: 100,
            messages_per_producer: 50000,
            notification_style: NotifyStyle::One,
            timeout: Timeout::Forever,
            delay_seconds: 0
        );
    }
}
