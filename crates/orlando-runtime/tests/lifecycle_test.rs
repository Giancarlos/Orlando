use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use orlando_runtime::Silo;

#[test]
fn startup_hook_runs_on_build() {
    let started = Arc::new(AtomicBool::new(false));
    let started_clone = started.clone();

    let _silo = Silo::builder()
        .on_startup(move || {
            started_clone.store(true, Ordering::SeqCst);
        })
        .build();

    assert!(started.load(Ordering::SeqCst));
}

#[test]
fn shutdown_hook_runs_on_shutdown() {
    let stopped = Arc::new(AtomicBool::new(false));
    let stopped_clone = stopped.clone();

    let silo = Silo::builder()
        .on_shutdown(move || {
            stopped_clone.store(true, Ordering::SeqCst);
        })
        .build();

    assert!(!stopped.load(Ordering::SeqCst));
    silo.run_shutdown_hooks();
    assert!(stopped.load(Ordering::SeqCst));
}

#[test]
fn multiple_hooks_run_in_order() {
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let o1 = order.clone();
    let o2 = order.clone();

    let _silo = Silo::builder()
        .on_startup(move || {
            o1.lock().unwrap().push(1);
        })
        .on_startup(move || {
            o2.lock().unwrap().push(2);
        })
        .build();

    assert_eq!(*order.lock().unwrap(), vec![1, 2]);
}
