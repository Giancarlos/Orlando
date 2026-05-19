//! Integration tests for the HTTP /healthz and /readyz endpoints.

use std::net::SocketAddr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use orlando_cluster::{HashRing, SiloAddress, health_server::run_health_server};
use tokio::sync::watch;

fn unused_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

async fn spawn_server(
    ring: Arc<ArcSwap<HashRing>>,
    probe: Option<orlando_cluster::StoreProbe>,
) -> (SocketAddr, watch::Sender<bool>) {
    let port = unused_port();
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let (tx, rx) = watch::channel(false);
    tokio::spawn(run_health_server(addr, ring, probe, rx));
    // Wait for the listener to come up
    for _ in 0..50 {
        if reqwest::get(format!("http://{}/healthz", addr)).await.is_ok() {
            return (addr, tx);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("health server failed to start");
}

#[tokio::test]
async fn healthz_always_returns_200() {
    let ring = Arc::new(ArcSwap::from_pointee(HashRing::new(150)));
    let (addr, _shutdown) = spawn_server(ring, None).await;
    let res = reqwest::get(format!("http://{}/healthz", addr)).await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn readyz_503_when_ring_empty() {
    let ring = Arc::new(ArcSwap::from_pointee(HashRing::new(150)));
    let (addr, _shutdown) = spawn_server(ring, None).await;
    let res = reqwest::get(format!("http://{}/readyz", addr)).await.unwrap();
    assert_eq!(res.status(), 503);
}

#[tokio::test]
async fn readyz_200_when_ring_has_member_and_no_probe() {
    let mut ring = HashRing::new(150);
    ring.add(SiloAddress {
        host: "127.0.0.1".into(),
        port: 7001,
        silo_id: "s1".into(),
    });
    let ring = Arc::new(ArcSwap::from_pointee(ring));
    let (addr, _shutdown) = spawn_server(ring, None).await;
    let res = reqwest::get(format!("http://{}/readyz", addr)).await.unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn readyz_503_when_store_probe_fails() {
    let mut ring = HashRing::new(150);
    ring.add(SiloAddress {
        host: "127.0.0.1".into(),
        port: 7002,
        silo_id: "s1".into(),
    });
    let ring = Arc::new(ArcSwap::from_pointee(ring));

    let probe: orlando_cluster::StoreProbe =
        Arc::new(|| Box::pin(async { Err("db unreachable".to_string()) }));

    let (addr, _shutdown) = spawn_server(ring, Some(probe)).await;
    let res = reqwest::get(format!("http://{}/readyz", addr)).await.unwrap();
    assert_eq!(res.status(), 503);
}
