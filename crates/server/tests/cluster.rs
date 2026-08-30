use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use protocol::codec::RespCodec;
use protocol::Frame;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

const NODE_IDS: [&str; 3] = ["shard-a", "shard-b", "shard-c"];
const SLOT_RANGES: [(u16, u16); 3] = [(0, 5460), (5461, 10922), (10923, 16383)];

/// Three independent `rocket-mem` nodes sharing one static topology. The `TempDir`s must stay
/// alive for as long as the nodes run -- they own each node's AOF and snapshot paths.
struct Cluster {
    _dirs: Vec<tempfile::TempDir>,
    addrs: Vec<String>,
}

impl Cluster {
    fn addr(&self, index: usize) -> &str {
        &self.addrs[index]
    }

    /// The index of the node that owns `slot`, per `SLOT_RANGES`.
    fn owner_index(&self, slot: u16) -> usize {
        SLOT_RANGES
            .iter()
            .position(|(first, last)| *first <= slot && slot <= *last)
            .expect("SLOT_RANGES covers the whole slot space")
    }

    /// Any node that does *not* own `slot` -- what a mis-routed client would hit.
    fn non_owner_index(&self, slot: u16) -> usize {
        (self.owner_index(slot) + 1) % 3
    }
}

/// Binds all three listeners first, so the config file can name the ephemeral ports the OS
/// actually assigned, then starts one node per listener with that same config text and its own
/// node id. Each node gets its own Engine/AofWriter/ReplicationHandle -- sharing any of them
/// would let a test pass for the wrong reason.
async fn spawn_3_shard_cluster() -> Cluster {
    let mut listeners = Vec::new();
    let mut addrs = Vec::new();
    for _ in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        addrs.push(listener.local_addr().unwrap().to_string());
        listeners.push(listener);
    }

    let config_text: String = (0..3)
        .map(|i| {
            format!(
                "{} {} {} {}\n",
                NODE_IDS[i], addrs[i], SLOT_RANGES[i].0, SLOT_RANGES[i].1
            )
        })
        .collect();

    let mut dirs = Vec::new();
    for (i, listener) in listeners.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Arc::new(engine::Engine::new());
        let aof = Arc::new(
            rocket_mem::aof::AofWriter::open(
                &dir.path().join("node.aof"),
                rocket_mem::aof::FsyncPolicy::Never,
            )
            .unwrap(),
        );
        let config = rocket_mem::cluster::ClusterConfig::parse(&config_text, NODE_IDS[i]).unwrap();
        let replication = Arc::new(
            rocket_mem::replication::ReplicationHandle::new(
                Arc::clone(&engine),
                dir.path().join("node.snapshot"),
            )
            .with_cluster(Arc::new(config)),
        );
        tokio::spawn(rocket_mem::serve(listener, engine, aof, replication));
        dirs.push(dir);
    }

    Cluster { _dirs: dirs, addrs }
}

async fn connect(addr: &str) -> Framed<TcpStream, RespCodec> {
    Framed::new(
        TcpStream::connect(addr).await.unwrap(),
        RespCodec::default(),
    )
}

/// Sends one command and returns its reply frame. Raw RESP rather than the `redis` crate so
/// these tests can assert the exact `-MOVED` slot and address text.
async fn send(framed: &mut Framed<TcpStream, RespCodec>, parts: &[&[u8]]) -> Frame {
    let frame = Frame::Array(
        parts
            .iter()
            .map(|p| Frame::Bulk(Bytes::copy_from_slice(p)))
            .collect(),
    );
    framed.send(frame).await.unwrap();
    framed.next().await.unwrap().unwrap()
}

/// Pulls the `host:port` out of a `-MOVED <slot> <addr>` error, the way a cluster-aware client
/// does before reconnecting.
fn moved_target(reply: &Frame) -> String {
    let Frame::Error(msg) = reply else {
        panic!("expected a MOVED error, got {reply:?}");
    };
    let mut parts = msg.split_whitespace();
    assert_eq!(parts.next(), Some("MOVED"), "not a MOVED error: {msg}");
    let _slot = parts.next().expect("MOVED reply has a slot");
    parts
        .next()
        .expect("MOVED reply has an address")
        .to_string()
}

#[tokio::test]
async fn every_node_reports_the_same_three_shard_topology() {
    let cluster = spawn_3_shard_cluster().await;
    for i in 0..3 {
        let mut c = connect(cluster.addr(i)).await;
        let Frame::Bulk(info) = send(&mut c, &[b"CLUSTER", b"INFO"]).await else {
            panic!("expected Bulk")
        };
        let info = String::from_utf8(info.to_vec()).unwrap();
        assert!(info.contains("cluster_enabled:1\r\n"), "{info}");
        assert!(info.contains("cluster_known_nodes:3\r\n"), "{info}");

        assert_eq!(
            send(&mut c, &[b"CLUSTER", b"MYID"]).await,
            Frame::Bulk(Bytes::from(NODE_IDS[i]))
        );

        let Frame::Array(shards) = send(&mut c, &[b"CLUSTER", b"SHARDS"]).await else {
            panic!("expected Array")
        };
        assert_eq!(shards.len(), 3);

        let Frame::Bulk(nodes) = send(&mut c, &[b"CLUSTER", b"NODES"]).await else {
            panic!("expected Bulk")
        };
        let nodes = String::from_utf8(nodes.to_vec()).unwrap();
        assert_eq!(nodes.lines().count(), 3, "{nodes}");
        // exactly one line -- this node's own -- carries the `myself` flag
        assert_eq!(
            nodes
                .lines()
                .filter(|l| l.contains("myself,master"))
                .count(),
            1,
            "{nodes}"
        );
        assert!(
            nodes.lines().nth(i).unwrap().contains("myself,master"),
            "node {i} should flag itself: {nodes}"
        );
    }
}
