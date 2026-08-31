use crate::aof::AofWriter;
use crate::connection::ClientGuard;
use crate::dispatcher;
use crate::replication::ReplicationHandle;
use engine::Engine;
use futures_util::{SinkExt, StreamExt};
use protocol::rmp::{MsgType, RmpCodec, RmpMessage};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio_util::codec::Framed;

/// Caps how many requests on one RMP connection can be mid-dispatch at once. Once the
/// cap is hit, the read loop's next `semaphore.acquire_owned().await` blocks -- it stops
/// reading more requests off the socket, which applies ordinary TCP backpressure to
/// whatever sent them, mirroring how RESP's sequential loop already gets backpressure
/// for free. Without this, a client that pipelines aggressively and never reads its
/// replies could make the server spawn unbounded tasks and queue unbounded encoded
/// replies in memory.
const MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION: usize = 256;

pub async fn serve(
    listener: TcpListener,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
) {
    let mut next_client_id: u64 = 1;
    loop {
        let (socket, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue, // a failed accept shouldn't take the whole listener down
        };
        let client_id = next_client_id;
        next_client_id += 1;
        tokio::spawn(handle_connection(
            socket,
            Arc::clone(&engine),
            Arc::clone(&aof),
            Arc::clone(&replication),
            client_id,
        ));
    }
}

async fn handle_connection(
    socket: tokio::net::TcpStream,
    engine: Arc<Engine>,
    aof: Arc<AofWriter>,
    replication: Arc<ReplicationHandle>,
    client_id: u64,
) {
    replication.connection_opened();
    let _client_guard = ClientGuard(Arc::clone(&replication));
    let framed = Framed::new(socket, RmpCodec);
    let (mut sink, mut stream) = framed.split();
    // ONE Session for this connection's whole lifetime, shared by every request spawned below --
    // this is what lets AUTH on one request be observed by a later, independently-spawned
    // request on the same connection. See
    // ../../docs/superpowers/plans/2026-08-31-sprint-8-plans/07-rmp-session-sharing.md.
    let session = Arc::new(dispatcher::Session::new());

    // Every spawned request-handling task below gets its own clone of `tx`; this loop's own
    // clone is dropped when the read loop ends. The writer task's `rx.recv()` only returns
    // `None` once every clone has dropped -- i.e. once every in-flight task has also finished
    // and sent (or failed to send) its reply -- so a client disconnecting mid-flight still gets
    // every reply that was already in progress written out before the connection fully closes.
    let (tx, mut rx) = mpsc::channel::<RmpMessage>(MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION);
    let semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION));

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                break; // the client went away; nothing left to do but stop writing
            }
        }
    });

    while let Some(next) = stream.next().await {
        let request = match next {
            Ok(msg) if msg.msg_type == MsgType::Request => msg,
            Ok(_) => break, // a stray Response from a misbehaving client
            Err(e) => {
                eprintln!("rmp decode error: {e}");
                break;
            }
        };
        // Blocks once MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION tasks are already mid-dispatch --
        // that's the backpressure: the read loop stops pulling more requests off the socket
        // until one finishes and its permit is released.
        let permit = match Arc::clone(&semaphore).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => break, // semaphore closed -- only happens if it were explicitly closed, which nothing here does
        };
        let engine = Arc::clone(&engine);
        let aof = Arc::clone(&aof);
        let replication = Arc::clone(&replication);
        let tx = tx.clone();
        let session = Arc::clone(&session);
        // Spawned, not awaited inline: the read loop must go straight back to decoding the next
        // request without waiting for this one's reply -- that's what makes multiple in-flight
        // requests on one connection possible at all.
        tokio::spawn(async move {
            let _permit = permit; // released (dropped) when this task ends, freeing a slot
            let reply = dispatcher::dispatch_and_log(
                &engine,
                &aof,
                &replication,
                request.frame,
                &session,
                client_id,
            );
            let _ = tx
                .send(RmpMessage {
                    request_id: request.request_id,
                    msg_type: MsgType::Response,
                    frame: reply,
                })
                .await; // bounded channel: send is now async
        });
    }
    drop(tx);
    let _ = writer.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use protocol::Frame;
    use tokio::net::TcpStream;

    fn test_aof() -> (tempfile::TempDir, Arc<AofWriter>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, crate::aof::FsyncPolicy::Never).unwrap();
        (dir, Arc::new(writer))
    }

    async fn spawn_test_server() -> (tempfile::TempDir, std::net::SocketAddr, Arc<Engine>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let replication = Arc::new(ReplicationHandle::default());
        tokio::spawn(serve(listener, Arc::clone(&engine), aof, replication));
        (dir, addr, engine)
    }

    /// Like `spawn_test_server`, but runs the server on its own dedicated OS thread and Tokio
    /// runtime rather than on the calling test's own runtime. Needed by
    /// `an_rmp_connection_saturated_with_slow_requests_eventually_serves_every_reply`: that test
    /// fills all 256 in-flight permits with `DEBUG SLEEP`s, whose blocking `std::thread::sleep`
    /// (plain `tokio::spawn`, not `spawn_blocking` -- see Finding 1/2's own rationale for why)
    /// occupies every one of the server runtime's worker threads at once. If the test's own
    /// reply-polling logic shared that runtime, it would be starved right alongside the read
    /// loop for as long as the sleeps run, making any timing assertion meaningless -- confirmed
    /// empirically while writing this test, replies "arriving" within a supposedly-tight window
    /// only because the whole runtime, polling logic included, was frozen and resumed as one
    /// burst. A merely large `worker_threads` count on a shared runtime works around that too,
    /// but spinning up 256+ real OS threads inside a test binary that runs many tests
    /// concurrently reliably exhausts the process's thread limit (`EAGAIN` from `pthread_create`
    /// -- also confirmed empirically). Isolating the server onto its own small thread pool,
    /// separate from the test's own runtime, sidesteps both problems: the test's polling logic
    /// keeps running on schedule no matter how starved the server's pool gets, and only a
    /// handful of extra OS threads are needed.
    ///
    /// Leaks its background thread and Tokio runtime: there is no shutdown handle, `JoinHandle`,
    /// or drop guard here, so the spawned thread (and the `serve` loop running on it) simply
    /// runs for the rest of the test binary's process lifetime. Harmless for the one test that
    /// currently calls this (the process exits once the suite finishes), but a future caller
    /// that invokes this more than a handful of times per test run should add real teardown
    /// rather than relying on that.
    fn spawn_isolated_test_server() -> (std::net::SocketAddr, tempfile::TempDir) {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let addr = std_listener.local_addr().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = TcpListener::from_std(std_listener).unwrap();
                let engine = Arc::new(Engine::new());
                let writer = AofWriter::open(&aof_path, crate::aof::FsyncPolicy::Never).unwrap();
                let aof = Arc::new(writer);
                let replication = Arc::new(ReplicationHandle::default());
                serve(listener, engine, aof, replication).await;
            });
        });
        (addr, dir)
    }

    async fn connect(addr: std::net::SocketAddr) -> Framed<TcpStream, RmpCodec> {
        Framed::new(TcpStream::connect(addr).await.unwrap(), RmpCodec)
    }

    fn command(args: &[&[u8]]) -> Frame {
        Frame::Array(
            args.iter()
                .map(|a| Frame::Bulk(Bytes::copy_from_slice(a)))
                .collect(),
        )
    }

    #[tokio::test]
    async fn set_then_get_round_trips_over_a_real_socket() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"foo", b"bar"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.request_id, 1);
        assert_eq!(reply.frame, Frame::Simple("OK".into()));

        con.send(RmpMessage {
            request_id: 2,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"foo"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.request_id, 2);
        assert_eq!(reply.frame, Frame::Bulk(Bytes::from_static(b"bar")));
    }

    #[tokio::test]
    async fn a_write_over_rmp_updates_the_same_engine_a_second_rmp_connection_reads_from() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut a = connect(addr).await;
        let mut b = connect(addr).await;

        a.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"k", b"v"]),
        })
        .await
        .unwrap();
        assert_eq!(
            a.next().await.unwrap().unwrap().frame,
            Frame::Simple("OK".into())
        );

        b.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"k"]),
        })
        .await
        .unwrap();
        assert_eq!(
            b.next().await.unwrap().unwrap().frame,
            Frame::Bulk(Bytes::from_static(b"v"))
        );
    }

    #[tokio::test]
    async fn two_requests_sent_without_waiting_both_get_correct_replies_regardless_of_order() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 10,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"a", b"1"]),
        })
        .await
        .unwrap();
        con.send(RmpMessage {
            request_id: 20,
            msg_type: MsgType::Request,
            frame: command(&[b"SET", b"b", b"2"]),
        })
        .await
        .unwrap();

        let mut replies = std::collections::HashMap::new();
        for _ in 0..2 {
            let reply = con.next().await.unwrap().unwrap();
            replies.insert(reply.request_id, reply.frame);
        }
        assert_eq!(replies[&10], Frame::Simple("OK".into()));
        assert_eq!(replies[&20], Frame::Simple("OK".into()));
    }

    #[tokio::test]
    async fn an_unknown_command_gets_the_same_error_shape_resp_would() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let mut con = connect(addr).await;

        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"NOTACOMMAND"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert!(matches!(reply.frame, Frame::Error(_)));
    }

    #[tokio::test]
    async fn serve_tracks_connected_rmp_clients_and_drops_the_count_on_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(ReplicationHandle::default());
        tokio::spawn(serve(listener, engine, aof, Arc::clone(&replication)));

        let mut con = connect(addr).await;
        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"PING"]),
        })
        .await
        .unwrap();
        con.next().await.unwrap().unwrap();
        assert_eq!(replication.connected_clients(), 1);
        assert_eq!(replication.total_connections(), 1);

        drop(con);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(replication.connected_clients(), 0);
        assert_eq!(replication.total_connections(), 1); // the lifetime total never drops
    }

    #[tokio::test]
    async fn more_than_the_in_flight_cap_concurrent_requests_all_still_succeed() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let con = std::sync::Arc::new(tokio::sync::Mutex::new(connect(addr).await));

        // 2x the cap, all fired without waiting for any individual reply first -- proves
        // the semaphore-based cap throttles (pauses reading more requests) rather than
        // ever dropping, corrupting, or deadlocking a request once the cap is in play.
        let total = MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION * 2;
        for i in 0..total as u64 {
            let mut con = con.lock().await;
            con.send(RmpMessage {
                request_id: i,
                msg_type: MsgType::Request,
                frame: command(&[b"PING"]),
            })
            .await
            .unwrap();
        }

        let mut seen = std::collections::HashSet::new();
        for _ in 0..total {
            let mut con = con.lock().await;
            let reply = con.next().await.unwrap().unwrap();
            assert_eq!(reply.frame, Frame::Simple("PONG".into()));
            seen.insert(reply.request_id);
        }
        assert_eq!(seen.len(), total);
    }

    // Proves the in-flight cap's actual mechanism -- the semaphore genuinely refuses a 257th
    // permit while 256 are held -- with zero thread-scheduling confound. An earlier version of
    // this coverage instead drove 256 real `DEBUG SLEEP` requests over a live RMP connection and
    // asserted a 257th (fast) request's reply didn't arrive within a short window. That looked
    // like a semaphore proof but wasn't: `DEBUG SLEEP` blocks a real worker thread (Finding 1/2's
    // own point), and an independent synthetic probe -- 256 `tokio::spawn`ed
    // `std::thread::sleep(1s)` tasks plus one more "fast" task on a 4-worker runtime, *with no
    // semaphore at all* -- reproduced the identical "fast task doesn't complete within 300ms"
    // signature in 7/7 trials, purely from worker-thread starvation. That test would have passed
    // identically with the semaphore deleted, so it was not evidence the cap does anything. This
    // synchronous, runtime-free test exercises the semaphore directly instead, which is the
    // actual mechanism `handle_connection`'s read loop relies on -- see
    // `an_rmp_connection_saturated_with_slow_requests_eventually_serves_every_reply` below for
    // what the network-level test was salvaged into proving instead.
    #[test]
    fn a_semaphore_sized_to_the_in_flight_cap_refuses_a_257th_permit_while_256_are_held() {
        let semaphore = Semaphore::new(MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION);
        let permits: Vec<_> = (0..MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION)
            .map(|_| {
                semaphore
                    .try_acquire()
                    .expect("should have a permit available")
            })
            .collect();
        assert_eq!(permits.len(), MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION);

        // The 257th must be refused immediately -- no blocking, no timing window, no dependency
        // on worker-thread scheduling.
        assert!(semaphore.try_acquire().is_err());

        // Freeing one permit makes exactly one more available, not more.
        drop(permits.into_iter().next().unwrap());
        assert!(semaphore.try_acquire().is_ok());
    }

    // Renamed and reframed after review: this does NOT prove the semaphore specifically gates
    // the 257th request (see the comment on
    // `a_semaphore_sized_to_the_in_flight_cap_refuses_a_257th_permit_while_256_are_held` above
    // for why a network-level timing assertion here is structurally confounded by worker-thread
    // starvation and can't tell "the cap is gating" apart from "the runtime is merely slow").
    // What this test still legitimately proves, combined with
    // `more_than_the_in_flight_cap_concurrent_requests_all_still_succeed`'s all-fast-PINGs
    // coverage: a connection saturated with genuinely slow, real, worker-thread-blocking
    // commands still correctly delivers every single reply -- none dropped, none corrupted, none
    // permanently stuck -- once the backlog drains, rather than hanging or losing a reply under
    // heavy blocking load.
    #[tokio::test]
    async fn an_rmp_connection_saturated_with_slow_requests_eventually_serves_every_reply() {
        let (addr, _dir) = spawn_isolated_test_server();
        let mut con = connect(addr).await;

        // Short (10ms) sleeps, not the 1s used while chasing the (retracted) timing proof above
        // -- long enough to guarantee these are still in flight when the fast request is sent,
        // short enough that draining all of them through the isolated runtime's 4 worker threads
        // keeps this test's wall time reasonable.
        for i in 0..MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION as u64 {
            con.send(RmpMessage {
                request_id: i,
                msg_type: MsgType::Request,
                frame: command(&[b"DEBUG", b"SLEEP", b"0.01"]),
            })
            .await
            .unwrap();
        }
        let fast_request_id = MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION as u64;
        con.send(RmpMessage {
            request_id: fast_request_id,
            msg_type: MsgType::Request,
            frame: command(&[b"PING"]),
        })
        .await
        .unwrap();

        let total = MAX_IN_FLIGHT_REQUESTS_PER_CONNECTION + 1;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..total {
            let reply = tokio::time::timeout(std::time::Duration::from_secs(10), con.next())
                .await
                .expect("every reply must eventually arrive, not hang forever")
                .unwrap()
                .unwrap();
            if reply.request_id == fast_request_id {
                assert_eq!(reply.frame, Frame::Simple("PONG".into()));
            } else {
                assert_eq!(reply.frame, Frame::Simple("OK".into()));
            }
            seen.insert(reply.request_id);
        }
        assert_eq!(seen.len(), total); // every reply arrived exactly once
    }

    #[tokio::test]
    async fn auth_on_one_rmp_request_is_visible_to_a_later_request_on_the_same_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(ReplicationHandle::default());
        replication
            .acl
            .set_user(
                "app",
                &[
                    Bytes::from_static(b"on"),
                    Bytes::from_static(b">pw"),
                    Bytes::from_static(b"allcommands"),
                    Bytes::from_static(b"allkeys"),
                ],
            )
            .unwrap();
        tokio::spawn(serve(listener, engine, aof, Arc::clone(&replication)));

        let mut con = connect(addr).await;

        // Denied before AUTH -- proves the gate is live on this connection at all.
        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"k"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(
            reply.frame,
            Frame::Error("NOAUTH Authentication required.".into())
        );

        // AUTH is its own independently-spawned request (a fresh tokio::spawn inside
        // handle_connection, same as every other request) -- awaited here before sending the next
        // one, per the documented rule that a client needing B to observe A's effect must await A
        // first (Sprint 7 spec's multiplexing caveat). This is exactly the scenario where the old
        // per-request `Session::new()` would have silently discarded the authentication.
        con.send(RmpMessage {
            request_id: 2,
            msg_type: MsgType::Request,
            frame: command(&[b"AUTH", b"app", b"pw"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.frame, Frame::Simple("OK".into()));

        // A third, independently-spawned request on the SAME connection -- must see request 2's
        // authentication.
        con.send(RmpMessage {
            request_id: 3,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"k"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(
            reply.frame,
            Frame::Null,
            "authenticated GET of a missing key, not NOAUTH"
        );
    }

    #[tokio::test]
    async fn a_second_rmp_connection_does_not_share_the_first_connections_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let engine = Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = Arc::new(ReplicationHandle::default());
        replication
            .acl
            .set_user(
                "app",
                &[
                    Bytes::from_static(b"on"),
                    Bytes::from_static(b">pw"),
                    Bytes::from_static(b"allcommands"),
                    Bytes::from_static(b"allkeys"),
                ],
            )
            .unwrap();
        tokio::spawn(serve(listener, engine, aof, Arc::clone(&replication)));

        let mut a = connect(addr).await;
        a.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"AUTH", b"app", b"pw"]),
        })
        .await
        .unwrap();
        assert_eq!(
            a.next().await.unwrap().unwrap().frame,
            Frame::Simple("OK".into())
        );

        let mut b = connect(addr).await; // a second, independent connection
        b.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"GET", b"k"]),
        })
        .await
        .unwrap();
        assert_eq!(
            b.next().await.unwrap().unwrap().frame,
            Frame::Error("NOAUTH Authentication required.".into()),
            "each connection must have its own Session, not a globally shared one"
        );
    }

    #[tokio::test]
    async fn the_server_survives_a_client_disconnecting_immediately() {
        let (_dir, addr, _engine) = spawn_test_server().await;
        let stream = TcpStream::connect(addr).await.unwrap();
        drop(stream);

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // a second, independent connection must still work
        let mut con = connect(addr).await;
        con.send(RmpMessage {
            request_id: 1,
            msg_type: MsgType::Request,
            frame: command(&[b"PING"]),
        })
        .await
        .unwrap();
        let reply = con.next().await.unwrap().unwrap();
        assert_eq!(reply.frame, Frame::Simple("PONG".into()));
    }
}
