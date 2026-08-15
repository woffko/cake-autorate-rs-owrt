//! Non-blocking, identity-bound live counter sampling for native Auto-Tune.

use super::full_autotune::AutotuneCaptureRequest;
use super::identity::monotonic_boot_ms;
use super::speedtest::{self, SpeedtestTrafficCounters};
use std::collections::VecDeque;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const NFT_COUNTER_TIMEOUT: Duration = Duration::from_millis(2_500);
const RATE_WINDOW: Duration = Duration::from_secs(1);
const RATE_MIN_SPAN: Duration = Duration::from_millis(800);
const MAX_RATE_HISTORY_SAMPLES: usize = 32;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AutotuneCounterRateWindow {
    pub download_kbps: f64,
    pub upload_kbps: f64,
    pub observed_start: Instant,
    pub observed_end: Instant,
    pub fresh: bool,
}

/// One physical, non-overlapping counter delta between consecutive reads.
///
/// Rate windows intentionally overlap to make load detection robust.  Volume
/// accounting must not consume those windows because doing so would count the
/// same bytes several times.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AutotuneCounterDelta {
    pub download_bytes: u64,
    pub upload_bytes: u64,
    pub observed_start: Instant,
    pub observed_end: Instant,
    /// True only when the two consecutive physical endpoints are close
    /// enough to attribute their byte delta to one bounded loaded phase.  It
    /// is not the single-use endpoint-attestation/transport freshness fence.
    pub within_maximum_span: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct AutotuneCounterObservation {
    pub rate_window: Option<AutotuneCounterRateWindow>,
    pub delta: Option<AutotuneCounterDelta>,
}

pub(crate) struct AutotuneCounterRateTracker {
    min_interval: Duration,
    maximum_delta_span: Duration,
    history: VecDeque<(Instant, SpeedtestTrafficCounters)>,
    last_read: Instant,
    last_sample: Option<AutotuneCounterRateWindow>,
}

impl AutotuneCounterRateTracker {
    pub(crate) fn new(interval_ms: u64) -> Self {
        let min_interval = Duration::from_millis(interval_ms.max(25));
        let maximum_delta_span = min_interval.saturating_mul(3);
        let now = Instant::now();
        Self {
            min_interval,
            maximum_delta_span,
            history: VecDeque::with_capacity(MAX_RATE_HISTORY_SAMPLES),
            last_read: now.checked_sub(min_interval).unwrap_or(now),
            last_sample: None,
        }
    }

    pub(crate) fn new_with_maximum_delta_span(
        interval_ms: u64,
        maximum_delta_span_ms: u64,
    ) -> Result<Self, String> {
        let min_interval = Duration::from_millis(interval_ms.max(25));
        let maximum_delta_span = Duration::from_millis(maximum_delta_span_ms);
        if maximum_delta_span < min_interval || maximum_delta_span > Duration::from_secs(60) {
            return Err("Auto-Tune counter maximum delta span is invalid".to_string());
        }
        let now = Instant::now();
        Ok(Self {
            min_interval,
            maximum_delta_span,
            history: VecDeque::with_capacity(MAX_RATE_HISTORY_SAMPLES),
            last_read: now.checked_sub(min_interval).unwrap_or(now),
            last_sample: None,
        })
    }

    pub(crate) fn reset(&mut self, now: Instant) {
        self.history.clear();
        self.last_read = now;
        self.last_sample = None;
    }

    pub(crate) fn sample_due(&self, now: Instant) -> bool {
        now.checked_duration_since(self.last_read)
            .is_some_and(|interval| interval >= self.min_interval)
    }

    pub(crate) fn cached_sample(&self) -> Option<AutotuneCounterRateWindow> {
        self.last_sample.map(|sample| AutotuneCounterRateWindow {
            fresh: false,
            ..sample
        })
    }

    #[cfg(test)]
    pub(crate) fn observe_counters(
        &mut self,
        now: Instant,
        current: Option<SpeedtestTrafficCounters>,
    ) -> Option<AutotuneCounterRateWindow> {
        self.observe_counters_with_delta(now, current).rate_window
    }

    pub(crate) fn observe_counters_with_delta(
        &mut self,
        now: Instant,
        current: Option<SpeedtestTrafficCounters>,
    ) -> AutotuneCounterObservation {
        let empty = || AutotuneCounterObservation {
            rate_window: None,
            delta: None,
        };
        let Some(current) = current else {
            self.reset(now);
            return empty();
        };
        let previous = self.history.back().copied();
        if previous.is_some_and(|(previous_at, previous)| {
            now <= previous_at
                || current.rx_bytes < previous.rx_bytes
                || current.tx_bytes < previous.tx_bytes
        }) {
            self.reset(now);
            self.history.push_back((now, current));
            return empty();
        }
        let delta = previous.map(|(previous_at, previous)| {
            let elapsed = now.saturating_duration_since(previous_at);
            AutotuneCounterDelta {
                download_bytes: current.rx_bytes.saturating_sub(previous.rx_bytes),
                upload_bytes: current.tx_bytes.saturating_sub(previous.tx_bytes),
                observed_start: previous_at,
                observed_end: now,
                within_maximum_span: elapsed <= self.maximum_delta_span,
            }
        });
        if delta.is_some_and(|value| !value.within_maximum_span) {
            // The current endpoint remains useful as the first point of a new
            // bounded sequence, and the rejected physical slice remains
            // visible to diagnostics.  It must not remain inside rolling
            // history: otherwise a later short delta could create a rate
            // window which silently bridges this forbidden gap.
            self.history.clear();
            self.last_sample = None;
        }
        self.last_read = now;
        self.history.push_back((now, current));
        if delta.is_some_and(|value| !value.within_maximum_span) {
            return AutotuneCounterObservation {
                rate_window: None,
                delta,
            };
        }
        if let Some(cutoff) = now.checked_sub(RATE_WINDOW) {
            while self.history.len() > 2
                && self
                    .history
                    .get(1)
                    .is_some_and(|(observed_at, _)| *observed_at <= cutoff)
            {
                self.history.pop_front();
            }
        }
        while self.history.len() > MAX_RATE_HISTORY_SAMPLES {
            self.history.pop_front();
        }
        let Some((oldest_at, oldest)) = self.history.front().copied() else {
            self.last_sample = None;
            return AutotuneCounterObservation {
                rate_window: None,
                delta,
            };
        };
        let elapsed_duration = now.saturating_duration_since(oldest_at);
        if elapsed_duration < RATE_MIN_SPAN {
            self.last_sample = None;
            return AutotuneCounterObservation {
                rate_window: None,
                delta,
            };
        }
        let elapsed = elapsed_duration.as_secs_f64();
        if !elapsed.is_finite() || elapsed <= 0.0 {
            self.reset(now);
            return empty();
        }
        let sample = AutotuneCounterRateWindow {
            download_kbps: current.rx_bytes.saturating_sub(oldest.rx_bytes) as f64 * 8.0
                / elapsed
                / 1_000.0,
            upload_kbps: current.tx_bytes.saturating_sub(oldest.tx_bytes) as f64 * 8.0
                / elapsed
                / 1_000.0,
            observed_start: oldest_at,
            observed_end: now,
            fresh: true,
        };
        self.last_sample = Some(sample);
        AutotuneCounterObservation {
            rate_window: Some(sample),
            delta,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.history.is_empty() && self.last_sample.is_none()
    }
}

#[derive(Debug)]
pub(crate) struct AutotuneCounterCompletion {
    pub request: AutotuneCaptureRequest,
    pub epoch: u64,
    pub completed_at: Instant,
    pub completed_boot_ms: u64,
    pub outcome: Result<Option<SpeedtestTrafficCounters>, String>,
}

#[derive(Clone)]
struct WorkerCancellation {
    shutdown: Arc<AtomicBool>,
    cancel_current: Arc<AtomicBool>,
}

impl WorkerCancellation {
    fn cancelled(&self) -> bool {
        self.shutdown.load(Ordering::Acquire) || self.cancel_current.load(Ordering::Acquire)
    }
}

type CounterReader = dyn Fn(
        &AutotuneCaptureRequest,
        &WorkerCancellation,
    ) -> Result<Option<SpeedtestTrafficCounters>, String>
    + Send
    + Sync
    + 'static;

struct CounterWakeGuard(Option<Arc<OwnedFd>>);

impl Drop for CounterWakeGuard {
    fn drop(&mut self) {
        crate::notify_pinger_wake(self.0.as_deref());
    }
}

pub(crate) struct AutotuneCounterSampler {
    request_tx: Option<SyncSender<(AutotuneCaptureRequest, u64)>>,
    result_rx: Receiver<AutotuneCounterCompletion>,
    worker: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    cancel_current: Arc<AtomicBool>,
    epoch: u64,
    in_flight: Option<(AutotuneCaptureRequest, u64)>,
    in_flight_invalidated: bool,
}

impl AutotuneCounterSampler {
    pub fn new() -> Result<Self, String> {
        Self::with_reader(Self::live_reader())
    }

    pub(crate) fn new_with_wake(wake: Arc<OwnedFd>) -> Result<Self, String> {
        Self::with_reader_and_wake(Self::live_reader(), Some(wake))
    }

    #[cfg(test)]
    pub(crate) fn with_test_reader_and_wake<F>(
        reader: F,
        wake: Arc<OwnedFd>,
    ) -> Result<Self, String>
    where
        F: Fn(&AutotuneCaptureRequest) -> Result<Option<SpeedtestTrafficCounters>, String>
            + Send
            + Sync
            + 'static,
    {
        Self::with_reader_and_wake(
            Arc::new(move |request, _cancellation| reader(request)),
            Some(wake),
        )
    }

    fn live_reader() -> Arc<CounterReader> {
        Arc::new(|request, cancellation| {
            speedtest::read_live_traffic_counters_bounded(
                &request.job_id,
                &request.worker_run_id,
                NFT_COUNTER_TIMEOUT,
                || cancellation.cancelled(),
            )
        })
    }

    fn with_reader(reader: Arc<CounterReader>) -> Result<Self, String> {
        Self::with_reader_and_wake(reader, None)
    }

    fn with_reader_and_wake(
        reader: Arc<CounterReader>,
        wake: Option<Arc<OwnedFd>>,
    ) -> Result<Self, String> {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let cancel_current = Arc::new(AtomicBool::new(false));
        let cancellation = WorkerCancellation {
            shutdown: Arc::clone(&shutdown),
            cancel_current: Arc::clone(&cancel_current),
        };
        let worker_wake = wake;
        let worker = thread::Builder::new()
            .name("cake-nft-counter".to_string())
            .spawn(move || {
                let _wake_on_exit = CounterWakeGuard(worker_wake.clone());
                while let Ok((request, epoch)) = request_rx.recv() {
                    let mut outcome = reader(&request, &cancellation);
                    let completed_at = Instant::now();
                    let completed_boot_ms = match monotonic_boot_ms() {
                        Ok(value) => value,
                        Err(error) => {
                            outcome =
                                Err(format!("speedtest-accounting-clock-unavailable: {error}"));
                            0
                        }
                    };
                    if result_tx
                        .send(AutotuneCounterCompletion {
                            request,
                            epoch,
                            completed_at,
                            completed_boot_ms,
                            outcome,
                        })
                        .is_err()
                    {
                        break;
                    }
                    crate::notify_pinger_wake(worker_wake.as_deref());
                }
            })
            .map_err(|error| format!("unable to start Auto-Tune counter worker: {error}"))?;
        Ok(Self {
            request_tx: Some(request_tx),
            result_rx,
            worker: Some(worker),
            shutdown,
            cancel_current,
            epoch: 1,
            in_flight: None,
            in_flight_invalidated: false,
        })
    }

    pub fn try_schedule(&mut self, request: &AutotuneCaptureRequest) -> Result<bool, String> {
        if self.in_flight.is_some() {
            return Ok(false);
        }
        let sender = self
            .request_tx
            .as_ref()
            .ok_or_else(|| "Auto-Tune counter worker is stopped".to_string())?;
        self.cancel_current.store(false, Ordering::Release);
        match sender.try_send((request.clone(), self.epoch)) {
            Ok(()) => {
                self.in_flight = Some((request.clone(), self.epoch));
                self.in_flight_invalidated = false;
                Ok(true)
            }
            Err(TrySendError::Full(_)) => {
                self.cancel_current.store(true, Ordering::Release);
                Err("Auto-Tune counter worker queue is unexpectedly full".to_string())
            }
            Err(TrySendError::Disconnected(_)) => {
                self.cancel_current.store(true, Ordering::Release);
                Err("Auto-Tune counter worker is unavailable".to_string())
            }
        }
    }

    pub fn try_take(&mut self) -> Result<Option<AutotuneCounterCompletion>, String> {
        match self.result_rx.try_recv() {
            Ok(result) => {
                if self.in_flight.as_ref().is_none_or(|(request, epoch)| {
                    request != &result.request || *epoch != result.epoch
                }) {
                    self.in_flight = None;
                    self.in_flight_invalidated = false;
                    return Err(
                        "Auto-Tune counter worker returned an unexpected identity".to_string()
                    );
                }
                self.in_flight = None;
                self.in_flight_invalidated = false;
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("Auto-Tune counter worker terminated unexpectedly".to_string())
            }
        }
    }

    pub fn invalidate_current(&mut self) {
        if self.in_flight.is_some() && !self.in_flight_invalidated {
            self.cancel_current.store(true, Ordering::Release);
            self.epoch = self.epoch.saturating_add(1);
            self.in_flight_invalidated = true;
        }
    }

    pub fn current_epoch(&self) -> u64 {
        self.epoch
    }

    pub(crate) fn has_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }
}

impl Drop for AutotuneCounterSampler {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.cancel_current.store(true, Ordering::Release);
        self.request_tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
    use crate::operations::protocol::SpeedtestDirection;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::sync::{Condvar, Mutex};

    fn request(seed: char, sequence: u32) -> AutotuneCaptureRequest {
        AutotuneCaptureRequest {
            capture_id: seed.to_string().repeat(32),
            job_id: "b".repeat(32),
            worker_run_id: "c".repeat(32),
            permit_id: "d".repeat(32),
            instance_name: "wan_sqm".to_string(),
            sequence,
            control_sequence: sequence,
            deadline_boot_ms: u64::MAX,
            phase: AutotuneCapturePhase::LoadedMeasurement,
            topology: MeasurementTopology::RawDownload,
            direction: Some(SpeedtestDirection::Download),
            candidate_dl_kbps: None,
            candidate_ul_kbps: Some(100_000),
            load_reference_kbps: Some(100_000),
            transport_baseline_us: Some(10_000),
            route_fingerprint: "e".repeat(64),
            sqm_fingerprint: "f".repeat(64),
        }
    }

    fn take_with_timeout(sampler: &mut AutotuneCounterSampler) -> AutotuneCounterCompletion {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(result) = sampler.try_take().unwrap() {
                return result;
            }
            assert!(Instant::now() < deadline, "counter worker result timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn rate_tracker_uses_the_same_bounded_window_for_bursty_controlled_traffic() {
        let mut tracker = AutotuneCounterRateTracker::new(200);
        let started = Instant::now();
        let points = [
            (0, 0),
            (200, 0),
            (400, 40_000_000),
            (600, 40_000_000),
            (800, 80_000_000),
            (1_000, 100_000_000),
        ];
        let mut loaded = None;
        for (millis, rx_bytes) in points {
            loaded = tracker.observe_counters(
                started + Duration::from_millis(millis),
                Some(SpeedtestTrafficCounters {
                    rx_bytes,
                    tx_bytes: rx_bytes / 100,
                }),
            );
        }
        let loaded = loaded.expect("one-second burst window should produce a rate");
        assert_eq!(loaded.observed_start, started);
        assert_eq!(loaded.observed_end, started + Duration::from_secs(1));
        assert!((loaded.download_kbps - 800_000.0).abs() < 0.1);
        assert!((loaded.upload_kbps - 8_000.0).abs() < 0.1);
        assert!(loaded.fresh);

        assert!(tracker
            .observe_counters(started + Duration::from_millis(1_200), None)
            .is_none());
        assert!(tracker.is_empty());
    }

    #[test]
    fn rate_tracker_rejects_counter_regression_without_reusing_cached_authority() {
        let mut tracker = AutotuneCounterRateTracker::new(200);
        let started = Instant::now();
        assert!(tracker
            .observe_counters(
                started,
                Some(SpeedtestTrafficCounters {
                    rx_bytes: 1_000,
                    tx_bytes: 2_000,
                }),
            )
            .is_none());
        assert!(tracker
            .observe_counters(
                started + Duration::from_secs(1),
                Some(SpeedtestTrafficCounters {
                    rx_bytes: 999,
                    tx_bytes: 2_001,
                }),
            )
            .is_none());
        assert!(tracker.cached_sample().is_none());
    }

    #[test]
    fn physical_deltas_are_non_overlapping_even_when_rate_windows_overlap() {
        let mut tracker = AutotuneCounterRateTracker::new(200);
        let started = Instant::now();
        let mut delta_download = 0_u64;
        let mut delta_upload = 0_u64;
        let mut rate_windows = 0_u32;
        for index in 0_u64..=8 {
            let observation = tracker.observe_counters_with_delta(
                started + Duration::from_millis(index * 200),
                Some(SpeedtestTrafficCounters {
                    rx_bytes: index * 10_000,
                    tx_bytes: index * 1_000,
                }),
            );
            if let Some(delta) = observation.delta {
                assert_eq!(
                    delta.observed_end.duration_since(delta.observed_start),
                    Duration::from_millis(200)
                );
                assert!(delta.within_maximum_span);
                delta_download += delta.download_bytes;
                delta_upload += delta.upload_bytes;
            }
            if observation.rate_window.is_some() {
                rate_windows += 1;
            }
        }
        assert_eq!(delta_download, 80_000);
        assert_eq!(delta_upload, 8_000);
        assert!(
            rate_windows >= 5,
            "the rolling windows must overlap in this fixture"
        );
    }

    #[test]
    fn explicit_physical_delta_span_is_independent_of_nominal_sampling_cadence() {
        let started = Instant::now();
        let counters = |rx_bytes| SpeedtestTrafficCounters {
            rx_bytes,
            tx_bytes: rx_bytes / 1_000,
        };
        let mut tracker =
            AutotuneCounterRateTracker::new_with_maximum_delta_span(200, 1_500).unwrap();
        assert!(tracker
            .observe_counters_with_delta(started, Some(counters(0)))
            .delta
            .is_none());
        let vm_median = tracker.observe_counters_with_delta(
            started + Duration::from_millis(840),
            Some(counters(40_000_000)),
        );
        assert!(vm_median.delta.unwrap().within_maximum_span);
        assert!(vm_median.rate_window.is_some());

        let vm_maximum = tracker.observe_counters_with_delta(
            started + Duration::from_millis(1_958),
            Some(counters(80_000_000)),
        );
        assert_eq!(
            vm_maximum
                .delta
                .unwrap()
                .observed_end
                .duration_since(vm_maximum.delta.unwrap().observed_start),
            Duration::from_millis(1_118)
        );
        assert!(vm_maximum.delta.unwrap().within_maximum_span);

        let over_bound = tracker.observe_counters_with_delta(
            started + Duration::from_millis(3_459),
            Some(counters(120_000_000)),
        );
        assert!(!over_bound.delta.unwrap().within_maximum_span);
        assert!(over_bound.rate_window.is_none());

        let short_after_gap = tracker.observe_counters_with_delta(
            started + Duration::from_millis(4_158),
            Some(counters(140_000_000)),
        );
        assert!(short_after_gap.delta.unwrap().within_maximum_span);
        assert!(
            short_after_gap.rate_window.is_none(),
            "a bounded delta cannot reuse rate history from before an over-bound gap"
        );
        let rebuilt = tracker.observe_counters_with_delta(
            started + Duration::from_millis(4_259),
            Some(counters(145_000_000)),
        );
        assert!(rebuilt.delta.unwrap().within_maximum_span);
        assert_eq!(
            rebuilt
                .rate_window
                .expect("new bounded endpoints should rebuild the rate window")
                .observed_start,
            started + Duration::from_millis(3_459)
        );
    }

    #[test]
    fn dispatch_is_non_blocking_and_allows_only_one_in_flight_request() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_gate = Arc::clone(&gate);
        let reader_entered = Arc::clone(&entered);
        let reader: Arc<CounterReader> = Arc::new(move |_, _| {
            let (lock, ready) = &*reader_entered;
            *lock.lock().unwrap() = true;
            ready.notify_all();
            let (lock, release) = &*reader_gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = release.wait(open).unwrap();
            }
            Ok(Some(SpeedtestTrafficCounters {
                rx_bytes: 10,
                tx_bytes: 20,
            }))
        });
        let mut sampler = AutotuneCounterSampler::with_reader(reader).unwrap();
        let first = request('a', 1);
        let second = request('f', 2);
        assert!(sampler.try_schedule(&first).unwrap());
        let (lock, ready) = &*entered;
        let mut active = lock.lock().unwrap();
        while !*active {
            active = ready.wait(active).unwrap();
        }
        drop(active);
        let started = Instant::now();
        assert!(!sampler.try_schedule(&second).unwrap());
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(sampler.has_in_flight());
        let (lock, release) = &*gate;
        *lock.lock().unwrap() = true;
        release.notify_all();
        let result = take_with_timeout(&mut sampler);
        assert_eq!(result.request, first);
        assert!(sampler.try_schedule(&second).unwrap());
    }

    #[test]
    fn completion_notifies_the_shared_reactor_wake() {
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_gate = Arc::clone(&gate);
        let reader_entered = Arc::clone(&entered);
        let reader: Arc<CounterReader> = Arc::new(move |_, _| {
            let (lock, ready) = &*reader_entered;
            *lock.lock().unwrap() = true;
            ready.notify_all();
            let (lock, release) = &*reader_gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = release.wait(open).unwrap();
            }
            Ok(Some(SpeedtestTrafficCounters {
                rx_bytes: 10,
                tx_bytes: 20,
            }))
        });
        let mut sampler =
            AutotuneCounterSampler::with_reader_and_wake(reader, Some(Arc::clone(&wake))).unwrap();
        let capture = request('a', 1);
        assert!(sampler.try_schedule(&capture).unwrap());
        let (lock, ready) = &*entered;
        let mut active = lock.lock().unwrap();
        while !*active {
            active = ready.wait(active).unwrap();
        }
        drop(active);

        let mut descriptor = libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        let (lock, release) = &*gate;
        *lock.lock().unwrap() = true;
        release.notify_all();
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 1_000) }, 1);
        assert_ne!(descriptor.revents & libc::POLLIN, 0);
        let completion = take_with_timeout(&mut sampler);
        assert_eq!(completion.request, capture);
    }

    #[test]
    fn worker_exit_notifies_the_shared_reactor_wake() {
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let reader: Arc<CounterReader> = Arc::new(|_, _| Ok(None));
        let sampler =
            AutotuneCounterSampler::with_reader_and_wake(reader, Some(Arc::clone(&wake))).unwrap();
        let mut descriptor = libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        drop(sampler);
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 1);
        assert_ne!(descriptor.revents & libc::POLLIN, 0);
    }

    #[test]
    fn invalidation_cancels_the_exact_in_flight_read_and_preserves_identity() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_entered = Arc::clone(&entered);
        let reader: Arc<CounterReader> = Arc::new(move |_, cancellation| {
            let (lock, ready) = &*reader_entered;
            *lock.lock().unwrap() = true;
            ready.notify_all();
            while !cancellation.cancelled() {
                thread::sleep(Duration::from_millis(2));
            }
            Err("cancelled-test-read".to_string())
        });
        let mut sampler = AutotuneCounterSampler::with_reader(reader).unwrap();
        let first = request('a', 1);
        assert!(sampler.try_schedule(&first).unwrap());
        let (lock, ready) = &*entered;
        let mut active = lock.lock().unwrap();
        while !*active {
            active = ready.wait(active).unwrap();
        }
        drop(active);
        sampler.invalidate_current();
        let invalidated_epoch = sampler.current_epoch();
        sampler.invalidate_current();
        assert_eq!(sampler.current_epoch(), invalidated_epoch);
        let result = take_with_timeout(&mut sampler);
        assert_eq!(result.request, first);
        assert!(result.epoch < sampler.current_epoch());
        assert_eq!(result.outcome.unwrap_err(), "cancelled-test-read");
    }

    #[test]
    fn drop_cancels_and_joins_an_active_worker() {
        let entered = Arc::new((Mutex::new(false), Condvar::new()));
        let reader_entered = Arc::clone(&entered);
        let reader: Arc<CounterReader> = Arc::new(move |_, cancellation| {
            let (lock, ready) = &*reader_entered;
            *lock.lock().unwrap() = true;
            ready.notify_all();
            while !cancellation.cancelled() {
                thread::sleep(Duration::from_millis(2));
            }
            Err("shutdown-test-read".to_string())
        });
        let mut sampler = AutotuneCounterSampler::with_reader(reader).unwrap();
        assert!(sampler.try_schedule(&request('a', 1)).unwrap());
        let (lock, ready) = &*entered;
        let mut active = lock.lock().unwrap();
        while !*active {
            active = ready.wait(active).unwrap();
        }
        drop(active);
        let started = Instant::now();
        drop(sampler);
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
