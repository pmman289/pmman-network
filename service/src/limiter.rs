use pmman_protocol::NodeRateLimit;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub enum Direction {
    Upload,
    Download,
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Per-proxy cancellation: stopping one proxy must not stop a shared limiter.
#[derive(Default)]
pub struct RateLimitCancellation {
    cancelled: Mutex<bool>,
    wake: Condvar,
}

impl RateLimitCancellation {
    pub fn cancel(&self) {
        *self.cancelled.lock().expect("rate cancellation poisoned") = true;
        self.wake.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.lock().expect("rate cancellation poisoned")
    }

    fn wait(&self, timeout: Duration) -> bool {
        let cancelled = self.cancelled.lock().expect("rate cancellation poisoned");
        let (cancelled, _) = self
            .wake
            .wait_timeout_while(cancelled, timeout, |cancelled| !*cancelled)
            .expect("rate cancellation poisoned");
        !*cancelled
    }
}

pub struct RateLimiter {
    limits: Mutex<NodeRateLimit>,
    upload: Mutex<Bucket>,
    download: Mutex<Bucket>,
}

impl RateLimiter {
    pub fn new(limits: NodeRateLimit) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            limits: Mutex::new(limits),
            upload: Mutex::new(Bucket {
                tokens: 0.0,
                at: now,
            }),
            download: Mutex::new(Bucket {
                tokens: 0.0,
                at: now,
            }),
        })
    }

    pub fn limits(&self) -> NodeRateLimit {
        *self.limits.lock().expect("rate limit poisoned")
    }

    pub fn set_limits(&self, limits: NodeRateLimit) {
        *self.limits.lock().expect("rate limit poisoned") = limits;
        let upload_capacity = capacity(bytes_per_second(limits.upload_mbps));
        let mut upload = self.upload.lock().expect("rate bucket poisoned");
        upload.tokens = upload.tokens.min(upload_capacity);
        let download_capacity = capacity(bytes_per_second(limits.download_mbps));
        let mut download = self.download.lock().expect("rate bucket poisoned");
        download.tokens = download.tokens.min(download_capacity);
    }

    /// Non-blocking admission for the P2P relay worker. A denied datagram is
    /// retried by the existing WireGuard repair queue; direct traffic never
    /// calls this method and is therefore never rate limited.
    pub fn try_take(&self, direction: Direction, bytes: usize) -> bool {
        let rate = bytes_per_second(self.limit(direction));
        if rate == 0.0 {
            return true;
        }
        let mut bucket = self.bucket(direction).lock().expect("rate bucket poisoned");
        refill(&mut bucket, rate);
        let needed = bytes as f64;
        if bucket.tokens >= needed {
            bucket.tokens -= needed;
            true
        } else {
            false
        }
    }

    /// Blocking admission for the dedicated node gateway proxy. Returns false
    /// on cancellation, without admitting the pending packet. No limiter lock
    /// is held while waiting, and cancellation wakes the wait immediately.
    /// Large packets reserve their cost in chunks within the normal bucket
    /// capacity. A cancelled packet may have reserved part of its cost already.
    pub fn wait_take(
        &self,
        direction: Direction,
        bytes: usize,
        cancellation: &RateLimitCancellation,
    ) -> bool {
        let mut remaining = bytes as f64;
        loop {
            if cancellation.is_cancelled() {
                return false;
            }
            let rate = bytes_per_second(self.limit(direction));
            if rate == 0.0 {
                return !cancellation.is_cancelled();
            }
            let wait = {
                let mut bucket = self.bucket(direction).lock().expect("rate bucket poisoned");
                refill(&mut bucket, rate);
                if cancellation.is_cancelled() {
                    return false;
                }
                // Reserve available credit as it accrues rather than requiring
                // one packet to fit in the bucket. This also keeps progress
                // intact when shared P2P admission or set_limits clamps tokens.
                let taken = bucket.tokens.min(remaining);
                bucket.tokens -= taken;
                remaining -= taken;
                if remaining == 0.0 {
                    return true;
                }
                Duration::from_secs_f64((remaining.min(capacity(rate)) / rate).min(1.0))
            };
            if !cancellation.wait(wait.max(Duration::from_millis(1))) {
                return false;
            }
        }
    }

    fn limit(&self, direction: Direction) -> u32 {
        let limits = self.limits();
        limit_for_direction(limits, direction)
    }

    fn bucket(&self, direction: Direction) -> &Mutex<Bucket> {
        match direction {
            Direction::Upload => &self.upload,
            Direction::Download => &self.download,
        }
    }
}

fn limit_for_direction(limits: NodeRateLimit, direction: Direction) -> u32 {
    match direction {
        Direction::Upload => limits.upload_mbps,
        Direction::Download => limits.download_mbps,
    }
}

fn bytes_per_second(mbps: u32) -> f64 {
    (mbps as f64) * 125_000.0
}

fn capacity(rate: f64) -> f64 {
    if rate == 0.0 {
        0.0
    } else {
        (rate * 0.1).max(16_384.0)
    }
}

fn refill(bucket: &mut Bucket, rate: f64) {
    let now = Instant::now();
    let elapsed = now.duration_since(bucket.at).as_secs_f64();
    bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity(rate));
    bucket.at = now;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn low_bandwidth_admits_maximum_datagram_and_charges_its_full_cost() {
        let limiter = RateLimiter::new(NodeRateLimit {
            upload_mbps: 1,
            download_mbps: 1,
        });
        let initial_at = limiter.upload.lock().unwrap().at;
        let cancellation = Arc::new(RateLimitCancellation::default());
        let shared_limiter = Arc::clone(&limiter);
        let (stop_updates, updates_stopped) = mpsc::channel::<()>();
        let updates = std::thread::spawn(move || {
            while matches!(
                updates_stopped.recv_timeout(Duration::from_millis(5)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                // Repeated heartbeat updates and shared P2P bucket refills
                // must not reset progress on a packet larger than capacity.
                shared_limiter.set_limits(NodeRateLimit {
                    upload_mbps: 1,
                    download_mbps: 1,
                });
                assert!(shared_limiter.try_take(Direction::Upload, 0));
                assert!(shared_limiter.try_take(Direction::Download, 0));
            }
        });
        let mut workers = Vec::new();
        let (done, completed) = mpsc::channel();
        for direction in [Direction::Upload, Direction::Download] {
            let limiter = Arc::clone(&limiter);
            let cancellation = Arc::clone(&cancellation);
            let done = done.clone();
            workers.push(std::thread::spawn(move || {
                let admitted = limiter.wait_take(direction, 65_535, &cancellation);
                let _ = done.send((admitted, initial_at.elapsed()));
            }));
        }
        for _ in 0..2 {
            let result = completed.recv_timeout(Duration::from_secs(3));
            if result.is_err() {
                cancellation.cancel();
            }
            let (admitted, elapsed) = result.unwrap();
            assert!(admitted, "large datagram was cancelled");
            // An empty 1 Mbps bucket needs about 524 ms for this datagram.
            assert!(elapsed >= Duration::from_secs_f64(65_535.0 / 125_000.0));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        drop(stop_updates);
        updates.join().unwrap();
    }

    #[test]
    fn cancellation_wakes_a_pending_large_datagram_in_both_directions() {
        for direction in [Direction::Upload, Direction::Download] {
            let limiter = RateLimiter::new(NodeRateLimit {
                upload_mbps: 1,
                download_mbps: 1,
            });
            let cancellation = Arc::new(RateLimitCancellation::default());
            let initial_at = limiter.bucket(direction).lock().unwrap().at;
            let worker_limiter = Arc::clone(&limiter);
            let worker_cancellation = Arc::clone(&cancellation);
            let (done, completed) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let admitted = worker_limiter.wait_take(direction, 65_535, &worker_cancellation);
                let _ = done.send(admitted);
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            // Observe the first refill so cancellation exercises an active wait.
            while limiter.bucket(direction).lock().unwrap().at == initial_at {
                assert!(Instant::now() < deadline, "limiter worker did not start");
                std::thread::yield_now();
            }
            cancellation.cancel();
            assert!(!completed.recv_timeout(Duration::from_millis(250)).unwrap());
            worker.join().unwrap();
            // Cancellation belongs to this waiter, not the shared limiter.
            limiter.set_limits(NodeRateLimit::default());
            assert!(limiter.wait_take(direction, 65_535, &RateLimitCancellation::default()));
        }
    }

    #[test]
    fn cancellation_before_wait_takes_priority_even_when_unlimited() {
        let cancellation = RateLimitCancellation::default();
        cancellation.cancel();
        for mbps in [0, 1] {
            let limiter = RateLimiter::new(NodeRateLimit {
                upload_mbps: mbps,
                download_mbps: mbps,
            });
            for direction in [Direction::Upload, Direction::Download] {
                assert!(!limiter.wait_take(direction, 65_535, &cancellation));
            }
        }
    }

    #[test]
    fn nonblocking_admission_keeps_the_existing_burst_capacity() {
        let limiter = RateLimiter::new(NodeRateLimit {
            upload_mbps: 1,
            download_mbps: 1,
        });
        for direction in [Direction::Upload, Direction::Download] {
            limiter.bucket(direction).lock().unwrap().at = Instant::now() - Duration::from_secs(1);
            assert!(!limiter.try_take(direction, 65_535));
            assert!(limiter.try_take(direction, 16_384));
            assert!(!limiter.try_take(direction, 16_384));
        }
    }

    #[test]
    fn zero_is_unlimited_for_both_directions() {
        let limiter = RateLimiter::new(NodeRateLimit::default());
        for _ in 0..128 {
            assert!(limiter.try_take(Direction::Upload, 1500));
            assert!(limiter.try_take(Direction::Download, 1500));
        }
    }

    #[test]
    fn upload_and_download_limits_are_independent() {
        let limits = NodeRateLimit {
            upload_mbps: 7,
            download_mbps: 19,
        };
        assert_eq!(limit_for_direction(limits, Direction::Upload), 7);
        assert_eq!(limit_for_direction(limits, Direction::Download), 19);
        assert_eq!(bytes_per_second(8), 1_000_000.0);
    }

    #[test]
    fn dynamic_update_can_remove_a_cap_without_restarting_the_limiter() {
        let limiter = RateLimiter::new(NodeRateLimit {
            upload_mbps: 1,
            download_mbps: 1,
        });
        limiter.set_limits(NodeRateLimit::default());
        for _ in 0..128 {
            assert!(limiter.try_take(Direction::Upload, 1500));
            assert!(limiter.try_take(Direction::Download, 1500));
        }
    }
}
