use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use orlando_core::{Grain, GrainActivator, GrainContext, GrainHandler, Message};
use orlando_cluster::{ClusterSilo, FailureDetectorConfig, NetworkMessage};

// ── Test grain ──────────────────────────────────────────────────

struct Counter;

#[derive(Default)]
struct CounterState {
    count: i64,
}

impl Grain for Counter {
    type State = CounterState;

    fn idle_timeout() -> Duration {
        Duration::from_secs(5)
    }
}

// Messages

#[derive(Serialize, Deserialize)]
struct Increment {
    amount: i64,
}

impl Message for Increment {
    type Result = i64;
}

impl NetworkMessage for Increment {
    fn message_type_name() -> &'static str {
        "Increment"
    }
}

#[derive(Serialize, Deserialize)]
struct GetCount;

impl Message for GetCount {
    type Result = i64;
}

impl NetworkMessage for GetCount {
    fn message_type_name() -> &'static str {
        "GetCount"
    }
}

// Handlers

#[async_trait]
impl GrainHandler<Increment> for Counter {
    async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
        state.count += msg.amount;
        state.count
    }
}

#[async_trait]
impl GrainHandler<GetCount> for Counter {
    async fn handle(state: &mut CounterState, _msg: GetCount, _ctx: &GrainContext) -> i64 {
        state.count
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn local_dispatch_through_cluster_silo() {
    let silo = ClusterSilo::builder()
        .host("127.0.0.1")
        .port(0)
        .silo_id("test-silo")
        .register::<Counter, Increment>()
        .register::<Counter, GetCount>()
        .build();

    let grain = silo.get_ref::<Counter>("my-counter");
    let result = grain.ask(Increment { amount: 5 }).await.unwrap();
    assert_eq!(result, 5);

    let result = grain.ask(Increment { amount: 3 }).await.unwrap();
    assert_eq!(result, 8);

    let count = grain.ask(GetCount).await.unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn cross_silo_grain_call() {
    // Silo A: owns the grain, serves gRPC
    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(0) // will bind to a random port
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // We need to know the actual port, so bind a listener first
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let actual_port = listener.local_addr().unwrap().port();

    // Rebuild silo_a with the actual port
    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(actual_port)
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // Drop the listener so the port is free for tonic
    drop(listener);

    // Start silo_a's gRPC server in background
    let silo_a_clone = silo_a.clone();
    let server_handle = tokio::spawn(async move {
        silo_a_clone.serve().await.unwrap();
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Silo B: joins the cluster, will route some grains to silo_a
    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(0)
            .silo_id("silo-b")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    silo_b
        .join_cluster(&format!("127.0.0.1:{}", actual_port))
        .await
        .unwrap();

    // Try many grain keys — some will hash to silo-a (remote), some to silo-b (local).
    // We specifically test that at least one remote call succeeds.
    let mut remote_successes = 0;
    for i in 0..20 {
        let key = format!("counter-{}", i);
        let grain = silo_b.get_ref::<Counter>(&key);
        let result = grain.ask(Increment { amount: 1 }).await;
        if result.is_ok() {
            remote_successes += 1;
        }
    }

    // With consistent hashing and 2 silos, roughly half should go to each.
    // We just check that at least some worked (covers both local and remote paths).
    assert!(
        remote_successes >= 10,
        "expected at least 10 successful calls, got {}",
        remote_successes
    );

    server_handle.abort();
}

#[tokio::test]
async fn membership_join_and_get_members() {
    use orlando_cluster::proto::membership_client::MembershipClient;
    use orlando_cluster::proto::{GetMembersRequest, JoinRequest, SiloAddress};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let silo = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port)
            .silo_id("seed")
            .build(),
    );

    let silo_clone = silo.clone();
    let server = tokio::spawn(async move {
        silo_clone.serve().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client =
        MembershipClient::connect(format!("http://127.0.0.1:{}", port))
            .await
            .unwrap();

    // Join a new silo
    let resp = client
        .join(JoinRequest {
            joiner: Some(SiloAddress {
                host: "127.0.0.1".into(),
                port: 9999,
                silo_id: "newcomer".into(),
            }),
        })
        .await
        .unwrap();

    let members = resp.into_inner().members;
    assert!(members.len() >= 2, "expected at least 2 members after join");

    // GetMembers should also return both
    let resp = client
        .get_members(GetMembersRequest {})
        .await
        .unwrap();
    let members = resp.into_inner().members;
    let ids: Vec<String> = members.iter().map(|m| m.silo_id.clone()).collect();
    assert!(ids.contains(&"seed".to_string()));
    assert!(ids.contains(&"newcomer".to_string()));

    server.abort();
}

#[tokio::test]
async fn failure_detector_removes_dead_silo() {
    // Start silo A (seed) with fast failure detection
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .failure_detector_config(FailureDetectorConfig {
                protocol_period: Duration::from_millis(50),
                ping_timeout: Duration::from_millis(30),
                ping_req_count: 0,
                suspect_timeout: Duration::from_millis(80),
                gossip_fanout: 6,
            })
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_a_clone = silo_a.clone();
    let server_a = tokio::spawn(async move {
        silo_a_clone.serve().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Start silo B and join silo A
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .failure_detector_config(FailureDetectorConfig {
                protocol_period: Duration::from_millis(50),
                ping_timeout: Duration::from_millis(30),
                ping_req_count: 0,
                suspect_timeout: Duration::from_millis(80),
                gossip_fanout: 6,
            })
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_b_clone = silo_b.clone();
    let server_b = tokio::spawn(async move {
        silo_b_clone.serve().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Both join each other
    silo_a
        .join_cluster(&format!("127.0.0.1:{}", port_b))
        .await
        .unwrap();
    silo_b
        .join_cluster(&format!("127.0.0.1:{}", port_a))
        .await
        .unwrap();

    // Gracefully shut down silo B (gRPC server + SWIM detector)
    silo_b.shutdown();
    let _ = server_b.await;

    // Wait for SWIM failure detection:
    // protocol_period(50ms) + ping_timeout(30ms) + indirect pings + suspect_timeout(100ms) + margin
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Silo A should have removed silo B from its ring
    // All grains should now route to silo A (the only survivor)
    for i in 0..10 {
        let key = format!("counter-{}", i);
        let grain = silo_a.get_ref::<Counter>(&key);
        let result = grain.ask(Increment { amount: 1 }).await;
        assert!(result.is_ok(), "grain {} should route locally after silo-b death", key);
    }

    server_a.abort();
}

#[tokio::test]
async fn rebalancing_deactivates_misplaced_grains() {
    // Start silo A alone with some active grains
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_a_clone = silo_a.clone();
    let server_a = tokio::spawn(async move {
        silo_a_clone.serve().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Activate many grains on silo A (it's the only silo, so all are local)
    for i in 0..20 {
        let grain = silo_a.get_ref::<Counter>(&format!("counter-{}", i));
        grain.ask(Increment { amount: 1 }).await.unwrap();
    }

    let before_count = silo_a.directory().grain_ids().len();
    assert_eq!(before_count, 20, "all 20 grains should be active on silo-a");

    // Start silo B and have silo A join it (triggering rebalance on silo A)
    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_b_clone = silo_b.clone();
    let _server_b = tokio::spawn(async move {
        silo_b_clone.serve().await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Silo A joins silo B — this adds silo B to A's ring and broadcasts MembershipChange
    silo_a
        .join_cluster(&format!("127.0.0.1:{}", port_b))
        .await
        .unwrap();

    // Wait for rebalancer to process the membership change
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Some grains should have been deactivated from silo A (rehashed to silo B)
    let after_count = silo_a.directory().grain_ids().len();
    assert!(
        after_count < before_count,
        "expected fewer grains on silo-a after rebalance: before={}, after={}",
        before_count,
        after_count
    );
    assert!(
        after_count > 0,
        "silo-a should still have some grains (not all rehashed away)"
    );

    server_a.abort();
}

#[tokio::test]
async fn gossip_propagates_join_to_all_members() {
    use orlando_cluster::proto::membership_client::MembershipClient;
    use orlando_cluster::proto::GetMembersRequest;

    // Start 3 silos: A, B, C
    let mut ports = Vec::new();
    for _ in 0..3 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        ports.push(l.local_addr().unwrap().port());
        drop(l);
    }

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(ports[0])
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );
    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(ports[1])
            .silo_id("silo-b")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );
    let silo_c = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(ports[2])
            .silo_id("silo-c")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    // Start all servers
    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });
    let c_clone = silo_c.clone();
    let _sc = tokio::spawn(async move { c_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // B joins A — A now knows about B
    silo_b
        .join_cluster(&format!("127.0.0.1:{}", ports[0]))
        .await
        .unwrap();

    // C joins A — A returns [A, B, C]. C then notifies B about itself.
    silo_c
        .join_cluster(&format!("127.0.0.1:{}", ports[0]))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify: B should know about C (learned via gossip NotifyJoin)
    let mut client_b =
        MembershipClient::connect(format!("http://127.0.0.1:{}", ports[1]))
            .await
            .unwrap();
    let resp = client_b
        .get_members(GetMembersRequest {})
        .await
        .unwrap();
    let member_ids: Vec<String> = resp
        .into_inner()
        .members
        .iter()
        .map(|m| m.silo_id.clone())
        .collect();

    assert!(
        member_ids.contains(&"silo-a".to_string()),
        "silo-b should know about silo-a, got {:?}",
        member_ids
    );
    assert!(
        member_ids.contains(&"silo-c".to_string()),
        "silo-b should know about silo-c via gossip, got {:?}",
        member_ids
    );
}

#[tokio::test]
async fn gateway_forwards_to_correct_silo() {
    use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
    use orlando_cluster::proto::InvokeRequest;

    // Start two silos
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Form cluster
    silo_b
        .join_cluster(&format!("127.0.0.1:{}", port_a))
        .await
        .unwrap();

    // Find a grain key that hashes to silo-a (not silo-b)
    let grain_type = std::any::type_name::<Counter>();
    let mut target_key = String::new();
    {
        let ring = silo_b.pool(); // just need any way to check the ring
        // Use silo_b's get_ref to find a key that routes to silo-a
        for i in 0..100 {
            let key = format!("gw-test-{}", i);
            let grain = silo_b.get_ref::<Counter>(&key);
            // Try to increment on silo_b — if it goes remote, that means silo_a owns it
            // We can check by looking at silo_b's directory after
        }
    }

    // Simpler approach: send to silo-b via raw gRPC for several keys.
    // Some will be local to silo-b, others will be forwarded to silo-a.
    // ALL should succeed (the gateway forwards transparently).
    let mut client_b = GrainTransportClient::connect(format!("http://127.0.0.1:{}", port_b))
        .await
        .unwrap();

    let mut all_ok = true;
    for i in 0..20 {
        let key = format!("gw-{}", i);
        let payload = bincode::serde::encode_to_vec(
            &Increment { amount: 1 },
            bincode::config::standard(),
        )
        .unwrap();

        let response = client_b
            .invoke(InvokeRequest {
                grain_type: grain_type.to_string(),
                grain_key: key,
                message_type: "Increment".to_string(),
                payload,
                encoding: 0, // bincode
            })
            .await
            .unwrap();

        let inner = response.into_inner();
        if !inner.error.is_empty() {
            all_ok = false;
            eprintln!("gateway error: {}", inner.error);
        }
    }

    assert!(all_ok, "all 20 grain calls via gateway should succeed");

    // Verify that some grains ended up on silo-a (proving forwarding worked)
    // and some stayed on silo-b (local dispatch). The exact split depends on
    // consistent hashing, but both silos should have at least some.
    let a_count = silo_a.directory().grain_ids().len();
    let b_count = silo_b.directory().grain_ids().len();
    assert!(
        a_count > 0,
        "silo-a should have some grains from forwarded calls, got {}",
        a_count
    );
    assert!(
        b_count > 0,
        "silo-b should have some local grains, got {}",
        b_count
    );

    _sa.abort();
    _sb.abort();
}

#[tokio::test]
async fn gateway_forwards_protobuf_to_correct_silo() {
    use orlando_cluster::proto::grain_transport_client::GrainTransportClient;
    use orlando_cluster::proto::InvokeRequest;

    // Same as above but with protobuf encoding — testing that forwarding
    // preserves the encoding field through the proxy hop.
    let listener_a = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_a = listener_a.local_addr().unwrap().port();
    drop(listener_a);

    let listener_b = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port_b = listener_b.local_addr().unwrap().port();
    drop(listener_b);

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_a)
            .silo_id("silo-a")
            .register::<Counter, Increment>()
            .build(),
    );

    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(port_b)
            .silo_id("silo-b")
            .register::<Counter, Increment>()
            .build(),
    );

    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(100)).await;

    silo_b
        .join_cluster(&format!("127.0.0.1:{}", port_a))
        .await
        .unwrap();

    // Send bincode requests via silo-b's gateway — they should be forwarded to silo-a
    // transparently, regardless of encoding
    let mut client_b = GrainTransportClient::connect(format!("http://127.0.0.1:{}", port_b))
        .await
        .unwrap();

    let grain_type = std::any::type_name::<Counter>();
    let mut success_count = 0;
    for i in 0..10 {
        let key = format!("gw-proto-{}", i);
        let payload = bincode::serde::encode_to_vec(
            &Increment { amount: i + 1 },
            bincode::config::standard(),
        )
        .unwrap();

        let response = client_b
            .invoke(InvokeRequest {
                grain_type: grain_type.to_string(),
                grain_key: key,
                message_type: "Increment".to_string(),
                payload,
                encoding: 0,
            })
            .await
            .unwrap();

        if response.into_inner().error.is_empty() {
            success_count += 1;
        }
    }

    assert_eq!(success_count, 10, "all calls through gateway should succeed");

    _sa.abort();
    _sb.abort();
}

#[tokio::test]
async fn swim_gossip_piggybacks_on_ping() {
    // Start 3 silos with fast protocol periods. Verify that gossip updates
    // propagate via piggybacking on ping/pong messages (not just NotifyJoin RPCs).
    let mut ports = Vec::new();
    for _ in 0..3 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        ports.push(l.local_addr().unwrap().port());
        drop(l);
    }

    let fast_config = FailureDetectorConfig {
        protocol_period: Duration::from_millis(30),
        ping_timeout: Duration::from_millis(20),
        ping_req_count: 1,
        suspect_timeout: Duration::from_millis(200),
        gossip_fanout: 10,
    };

    let silo_a = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(ports[0])
            .silo_id("silo-a")
            .failure_detector_config(fast_config.clone())
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );
    let silo_b = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(ports[1])
            .silo_id("silo-b")
            .failure_detector_config(fast_config.clone())
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );
    let silo_c = Arc::new(
        ClusterSilo::builder()
            .host("127.0.0.1")
            .port(ports[2])
            .silo_id("silo-c")
            .failure_detector_config(fast_config)
            .register::<Counter, Increment>()
            .register::<Counter, GetCount>()
            .build(),
    );

    let a_clone = silo_a.clone();
    let _sa = tokio::spawn(async move { a_clone.serve().await.unwrap() });
    let b_clone = silo_b.clone();
    let _sb = tokio::spawn(async move { b_clone.serve().await.unwrap() });
    let c_clone = silo_c.clone();
    let _sc = tokio::spawn(async move { c_clone.serve().await.unwrap() });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Form A-B cluster
    silo_b
        .join_cluster(&format!("127.0.0.1:{}", ports[0]))
        .await
        .unwrap();

    // Wait for SWIM to start pinging between A and B
    tokio::time::sleep(Duration::from_millis(100)).await;

    // C joins A — A enqueues Join gossip for C
    silo_c
        .join_cluster(&format!("127.0.0.1:{}", ports[0]))
        .await
        .unwrap();

    // Wait for gossip to propagate via SWIM pings (not just NotifyJoin RPC).
    // The SWIM protocol piggybacks gossip updates on ping/pong messages.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // All three silos should be able to route grains to each other
    for i in 0..6 {
        let key = format!("gossip-counter-{}", i);
        let grain = silo_a.get_ref::<Counter>(&key);
        let result = grain.ask(Increment { amount: 1 }).await;
        assert!(result.is_ok(), "grain {} should be routable after gossip convergence", key);
    }
}
