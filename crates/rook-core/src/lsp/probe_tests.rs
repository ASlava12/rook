#![cfg(test)]
use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::{Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

#[test]
fn pending_probes_overlap_at_four_and_are_each_admitted_once() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (entered, starts) = mpsc::sync_channel(5);
    let worker_gate = gate.clone();
    let worker_active = active.clone();
    let worker_peak = peak.clone();
    let running = std::thread::spawn(move || {
        probe_candidates(&known(), 99, |candidate| {
            let current = worker_active.fetch_add(1, Ordering::SeqCst) + 1;
            worker_peak.fetch_max(current, Ordering::SeqCst);
            entered.send(candidate.language.clone()).unwrap();
            let (lock, wake) = &*worker_gate;
            let released = lock.lock().unwrap();
            let (released, timeout) =
                wake.wait_timeout_while(released, Duration::from_secs(30), |ready| !*ready).unwrap();
            worker_active.fetch_sub(1, Ordering::SeqCst);
            (*released && !timeout.timed_out()).then(|| candidate.clone())
        })
    });
    let until = Instant::now() + Duration::from_secs(30);
    let first: Vec<_> =
        (0..4).map(|_| starts.recv_timeout(until.saturating_duration_since(Instant::now()))).collect();
    // Release before asserting, so failed admission cannot strand test workers.
    {
        let (lock, wake) = &*gate;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    let found = running.join().unwrap();
    assert!(first.iter().all(Result::is_ok), "four probes must enter before release: {first:?}");
    assert_eq!(peak.load(Ordering::SeqCst), 4);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let mut calls: Vec<_> = first.into_iter().map(Result::unwrap).chain(starts).collect();
    calls.sort();
    let mut expected: Vec<_> = known().into_iter().map(|server| server.language).collect();
    expected.sort();
    assert_eq!(calls, expected, "missing or duplicate admissions");
    assert_eq!(
        found.into_iter().map(|server| server.language).collect::<Vec<_>>(),
        known().into_iter().map(|server| server.language).collect::<Vec<_>>()
    );
}

#[test]
fn empty_and_partial_discovery_keep_refusals_and_candidate_order() {
    assert!(probe_candidates(&[], 4, |_| panic!("empty batch cannot probe")).is_empty());
    let candidates = known();
    let expected: Vec<_> = candidates
        .iter()
        .filter(|server| server.language != "python")
        .map(|server| server.language.clone())
        .collect();
    for parallelism in [0, 1, 4, 99] {
        let calls = AtomicUsize::new(0);
        let found = probe_candidates(&candidates, parallelism, |candidate| {
            calls.fetch_add(1, Ordering::SeqCst);
            (candidate.language != "python").then(|| candidate.clone())
        });
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        assert_eq!(found.into_iter().map(|server| server.language).collect::<Vec<_>>(), expected);
    }
}

#[test]
fn later_completion_cannot_reorder_discovered_servers() {
    let candidates = known();
    let gate = (Mutex::new(false), Condvar::new());
    let completion = Mutex::new(Vec::new());
    let found = probe_candidates(&candidates[..2], 4, |candidate| {
        if candidate.language == "rust" {
            let ready = gate.0.lock().unwrap();
            let (ready, timeout) =
                gate.1.wait_timeout_while(ready, Duration::from_secs(30), |ready| !*ready).unwrap();
            if !*ready || timeout.timed_out() {
                return None;
            }
            completion.lock().unwrap().push(candidate.language.clone());
        } else {
            completion.lock().unwrap().push(candidate.language.clone());
            *gate.0.lock().unwrap() = true;
            gate.1.notify_all();
        }
        Some(candidate.clone())
    });
    assert_eq!(*completion.lock().unwrap(), ["typescript", "rust"]);
    assert_eq!(found.into_iter().map(|server| server.language).collect::<Vec<_>>(), ["rust", "typescript"]);
}
