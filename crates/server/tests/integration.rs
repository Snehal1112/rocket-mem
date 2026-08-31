use redis::AsyncCommands;
use std::sync::Arc;
use tokio::net::TcpListener;

async fn spawn_test_server() -> (tempfile::TempDir, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let engine = Arc::new(engine::Engine::new());
    let dir = tempfile::tempdir().unwrap();
    let aof = Arc::new(
        rocket_mem::aof::AofWriter::open(
            &dir.path().join("test.aof"),
            rocket_mem::aof::FsyncPolicy::Never,
        )
        .unwrap(),
    );
    tokio::spawn(rocket_mem::serve(
        listener,
        engine,
        aof,
        Arc::new(rocket_mem::replication::ReplicationHandle::default()),
    ));
    (dir, format!("redis://{addr}"))
}

#[tokio::test]
async fn redis_rs_client_can_set_and_get_over_real_tcp() {
    let (_dir, url) = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("foo", "bar").await.unwrap();
    let value: String = con.get("foo").await.unwrap();
    assert_eq!(value, "bar");
}

#[tokio::test]
async fn redis_rs_client_runs_a_mixed_workload_across_all_sprint_1_data_types() {
    let (_dir, url) = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("str", "hello").await.unwrap();
    let _: i64 = con.incr("counter", 5).await.unwrap();
    let _: () = con.hset("hash", "field", "value").await.unwrap();
    let _: () = con.rpush("list", "a").await.unwrap();
    let _: () = con.sadd("set", "member").await.unwrap();

    let str_val: String = con.get("str").await.unwrap();
    let hash_val: String = con.hget("hash", "field").await.unwrap();
    let is_member: bool = con.sismember("set", "member").await.unwrap();

    assert_eq!(str_val, "hello");
    assert_eq!(hash_val, "value");
    assert!(is_member);
}

#[tokio::test]
async fn redis_rs_client_gets_a_real_error_on_wrongtype() {
    let (_dir, url) = spawn_test_server().await;
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();

    let _: () = con.set("k", "v").await.unwrap();
    let result: redis::RedisResult<()> = con.hset("k", "f", "v").await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("WRONGTYPE"));
}

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[tokio::test]
async fn malformed_resp_input_gets_a_graceful_disconnect_not_a_crash() {
    let (_dir, url) = spawn_test_server().await;
    let addr = url.strip_prefix("redis://").unwrap();

    let mut raw = TcpStream::connect(addr).await.unwrap();
    raw.write_all(b"@this is not RESP\r\n").await.unwrap();
    // the connection should close (EOF), not hang or echo garbage back
    let mut buf = [0u8; 16];
    let n = raw.read(&mut buf).await.unwrap();
    assert_eq!(n, 0, "expected EOF on malformed input, got {n} bytes back");

    // the server itself must still be up — a fresh, well-formed connection works fine
    let client = redis::Client::open(url).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let _: () = redis::AsyncCommands::set(&mut con, "still-alive", "yes")
        .await
        .unwrap();
}

/// Minimal RESP2+RESP3 frame-length scanner used only by the test below. `protocol::codec::
/// RespCodec`'s `Decoder` impl understands RESP2 wire syntax (`+ - : $ *`) but has no arm for
/// the RESP3-only `%` (map) and `_` (null) type bytes -- even though its `Encoder` impl produces
/// exactly those once a session has negotiated RESP3. That's a real, pre-existing gap unrelated
/// to Session/connection wiring, so decoding this test's replies through `Framed<_, RespCodec>`
/// isn't an option; this scanner does the minimal byte-accounting needed to read one complete
/// frame of any of those types directly off the socket instead. Returns the frame's total byte
/// length, or `None` if `buf` doesn't yet hold a complete frame.
fn resp_frame_len(buf: &[u8]) -> Option<usize> {
    fn find_crlf(buf: &[u8]) -> Option<usize> {
        buf.windows(2).position(|w| w == b"\r\n")
    }
    if buf.is_empty() {
        return None;
    }
    match buf[0] {
        b'+' | b'-' | b':' => find_crlf(&buf[1..]).map(|crlf| 1 + crlf + 2),
        b'_' => (buf.len() >= 3).then_some(3),
        b'$' => {
            let crlf = find_crlf(&buf[1..])?;
            let len: i64 = std::str::from_utf8(&buf[1..1 + crlf]).ok()?.parse().ok()?;
            let header_len = 1 + crlf + 2;
            if len == -1 {
                return Some(header_len);
            }
            let total = header_len + len as usize + 2;
            (buf.len() >= total).then_some(total)
        }
        b'*' | b'%' => {
            let crlf = find_crlf(&buf[1..])?;
            let count: i64 = std::str::from_utf8(&buf[1..1 + crlf]).ok()?.parse().ok()?;
            let elements = if buf[0] == b'%' {
                count.max(0) * 2
            } else {
                count.max(0)
            };
            let mut consumed = 1 + crlf + 2;
            for _ in 0..elements {
                consumed += resp_frame_len(&buf[consumed..])?;
            }
            Some(consumed)
        }
        other => panic!("test frame scanner doesn't understand RESP type byte {other:#x}"),
    }
}

/// Reads exactly one complete RESP frame's raw bytes off `stream`, buffering partial reads in
/// `acc` (and leaving any bytes past the frame's end in `acc` for the next call).
async fn read_one_resp_frame(stream: &mut TcpStream, acc: &mut Vec<u8>) -> Vec<u8> {
    loop {
        if let Some(len) = resp_frame_len(acc) {
            return acc.drain(..len).collect();
        }
        let mut chunk = [0u8; 4096];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "connection closed before a full frame arrived");
        acc.extend_from_slice(&chunk[..n]);
    }
}

#[tokio::test]
async fn session_state_persists_across_sequential_requests_on_one_resp_connection() {
    let (_dir, url) = spawn_test_server().await;
    let addr = url.strip_prefix("redis://").unwrap();
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut acc = Vec::new();

    // Negotiate RESP3 -- this mutates the connection's Session, not just this one reply.
    stream
        .write_all(b"*2\r\n$5\r\nHELLO\r\n$1\r\n3\r\n")
        .await
        .unwrap();
    let hello_reply = read_one_resp_frame(&mut stream, &mut acc).await;
    // Confirms the negotiation actually took wire-level effect immediately: HELLO's own reply
    // is already sent as a native RESP3 map (`%...`), not the RESP2-flattened array (`*...`)
    // it would be for a session still on RESP2.
    assert_eq!(
        hello_reply.first(),
        Some(&b'%'),
        "expected HELLO 3's own reply to be RESP3-map-encoded, got {hello_reply:?}"
    );

    // A second, independent command on the SAME connection. If Session's protocol field didn't
    // persist (e.g. a regression back to a fresh Protocol::default() per request), this GET's
    // Null reply would encode as RESP2's `$-1\r\n` instead of RESP3's `_\r\n`.
    stream
        .write_all(b"*2\r\n$3\r\nGET\r\n$11\r\nmissing-key\r\n")
        .await
        .unwrap();
    let get_reply = read_one_resp_frame(&mut stream, &mut acc).await;
    assert_eq!(
        get_reply, b"_\r\n",
        "expected RESP3's `_\\r\\n` null encoding on this later, independent request -- proves \
         the Session's protocol negotiated by HELLO 3 persisted across the two requests on this \
         one connection rather than resetting to a fresh Protocol::default()"
    );
}
