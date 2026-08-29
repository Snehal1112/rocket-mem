use bytes::Bytes;
use engine::{commands, Engine, Value};
use protocol::codec::Protocol;
use protocol::Frame;

/// Extracts the `Vec<Bytes>` command name+args from an `Array` of `Bulk` frames —
/// the only shape a real RESP client ever sends a command as.
fn frame_to_args(frame: Frame) -> Result<Vec<Bytes>, Frame> {
    let Frame::Array(items) = frame else {
        return Err(Frame::Error(
            "ERR invalid request, expected array of bulk strings".into(),
        ));
    };
    items
        .into_iter()
        .map(|item| match item {
            Frame::Bulk(b) => Ok(b),
            _ => Err(Frame::Error(
                "ERR invalid request, expected array of bulk strings".into(),
            )),
        })
        .collect()
}

fn engine_error_to_frame(e: common::EngineError) -> Frame {
    Frame::Error(e.to_string())
}

macro_rules! require_args {
    ($rest:expr, $n:expr, $name:expr) => {
        if $rest.len() < $n {
            return Frame::Error(format!(
                "ERR wrong number of arguments for '{}' command",
                $name
            ));
        }
    };
}

pub fn dispatch(engine: &Engine, frame: Frame, protocol: &mut Protocol, client_id: u64) -> Frame {
    let args = match frame_to_args(frame) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if args.is_empty() {
        return Frame::Error("ERR empty command".into());
    }
    let name = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    let rest = &args[1..];

    match name.as_str() {
        "GET" => {
            require_args!(rest, 1, "get");
            match commands::string::get(engine, &rest[0]) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SET" => {
            require_args!(rest, 2, "set");
            let key = rest[0].clone();
            let val = rest[1].clone();
            let flags: Vec<String> = rest[2..]
                .iter()
                .map(|b| String::from_utf8_lossy(b).to_ascii_uppercase())
                .collect();
            if flags.iter().any(|f| f == "EX" || f == "PX") {
                return Frame::Error(
                    "ERR syntax error: EX/PX are not supported yet (planned Sprint 4)".into(),
                );
            }
            if flags.iter().any(|f| f == "NX") {
                if commands::string::set_nx(engine, key, val) {
                    Frame::Simple("OK".into())
                } else {
                    Frame::Null
                }
            } else if flags.iter().any(|f| f == "XX") {
                if commands::string::set_xx(engine, key, val) {
                    Frame::Simple("OK".into())
                } else {
                    Frame::Null
                }
            } else {
                engine.set(key, Value::String(val));
                Frame::Simple("OK".into())
            }
        }
        "APPEND" => {
            require_args!(rest, 2, "append");
            match commands::string::append(engine, rest[0].clone(), &rest[1]) {
                Ok(len) => Frame::Integer(len as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "STRLEN" => {
            require_args!(rest, 1, "strlen");
            match commands::string::strlen(engine, &rest[0]) {
                Ok(len) => Frame::Integer(len as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "INCR" => {
            require_args!(rest, 1, "incr");
            match commands::string::incr_by(engine, rest[0].clone(), 1) {
                Ok(n) => Frame::Integer(n),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "DECR" => {
            require_args!(rest, 1, "decr");
            match commands::string::incr_by(engine, rest[0].clone(), -1) {
                Ok(n) => Frame::Integer(n),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HSET" => {
            require_args!(rest, 3, "hset");
            match commands::hash::hset(engine, rest[0].clone(), rest[1].clone(), rest[2].clone()) {
                Ok(()) => Frame::Integer(1),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HGET" => {
            require_args!(rest, 2, "hget");
            match commands::hash::hget(engine, &rest[0], &rest[1]) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "INCRBY" => {
            require_args!(rest, 2, "incrby");
            let delta: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            match commands::string::incr_by(engine, rest[0].clone(), delta) {
                Ok(n) => Frame::Integer(n),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HDEL" => {
            require_args!(rest, 2, "hdel");
            match commands::hash::hdel(engine, &rest[0], &rest[1]) {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HGETALL" => {
            require_args!(rest, 1, "hgetall");
            match commands::hash::hgetall(engine, &rest[0]) {
                Ok(map) => Frame::Array(
                    map.into_iter()
                        .flat_map(|(f, v)| [Frame::Bulk(f), Frame::Bulk(v)])
                        .collect(),
                ),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HEXISTS" => {
            require_args!(rest, 2, "hexists");
            match commands::hash::hexists(engine, &rest[0], &rest[1]) {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HLEN" => {
            require_args!(rest, 1, "hlen");
            match commands::hash::hlen(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "RPUSH" => {
            require_args!(rest, 2, "rpush");
            match commands::list::rpush(engine, rest[0].clone(), rest[1].clone()) {
                Ok(()) => match commands::list::llen(engine, &rest[0]) {
                    Ok(n) => Frame::Integer(n as i64),
                    Err(e) => engine_error_to_frame(e),
                },
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LPUSH" => {
            require_args!(rest, 2, "lpush");
            match commands::list::lpush(engine, rest[0].clone(), rest[1].clone()) {
                Ok(()) => match commands::list::llen(engine, &rest[0]) {
                    Ok(n) => Frame::Integer(n as i64),
                    Err(e) => engine_error_to_frame(e),
                },
                Err(e) => engine_error_to_frame(e),
            }
        }
        "RPOP" => {
            require_args!(rest, 1, "rpop");
            match commands::list::rpop(engine, &rest[0]) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LPOP" => {
            require_args!(rest, 1, "lpop");
            match commands::list::lpop(engine, &rest[0]) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LLEN" => {
            require_args!(rest, 1, "llen");
            match commands::list::llen(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LRANGE" => {
            require_args!(rest, 3, "lrange");
            let (start, stop) = match (
                std::str::from_utf8(&rest[1])
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok()),
                std::str::from_utf8(&rest[2])
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok()),
            ) {
                (Some(a), Some(b)) => (a, b),
                _ => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            match commands::list::lrange(engine, &rest[0], start, stop) {
                Ok(items) => Frame::Array(items.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SADD" => {
            require_args!(rest, 2, "sadd");
            match commands::set::sadd(engine, rest[0].clone(), rest[1].clone()) {
                Ok(()) => Frame::Integer(1),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SREM" => {
            require_args!(rest, 2, "srem");
            match commands::set::srem(engine, &rest[0], &rest[1]) {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SMEMBERS" => {
            require_args!(rest, 1, "smembers");
            match commands::set::smembers(engine, &rest[0]) {
                Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SISMEMBER" => {
            require_args!(rest, 2, "sismember");
            match commands::set::sismember(engine, &rest[0], &rest[1]) {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SCARD" => {
            require_args!(rest, 1, "scard");
            match commands::set::scard(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "PING" => match rest.first() {
            Some(msg) => Frame::Bulk(msg.clone()),
            None => Frame::Simple("PONG".into()),
        },
        "ECHO" => {
            require_args!(rest, 1, "echo");
            Frame::Bulk(rest[0].clone())
        }
        "SELECT" => Frame::Simple("OK".into()), // single logical DB only, per 2026-08-29-sprint-2-spec.md scope
        "COMMAND" => Frame::Array(vec![]), // enough that clients probing capabilities don't choke
        "INFO" => Frame::Bulk(Bytes::from(format!(
            "# Server\r\nredis_version:rocket-mem-{}\r\n",
            env!("CARGO_PKG_VERSION")
        ))),
        "HELLO" => match rest.first() {
            None => hello_reply(*protocol, client_id),
            Some(arg) => match arg.as_ref() {
                b"2" => {
                    if rest.len() > 1 {
                        return Frame::Error("ERR syntax error".into());
                    }
                    *protocol = Protocol::Resp2;
                    hello_reply(*protocol, client_id)
                }
                b"3" => {
                    if rest.len() > 1 {
                        return Frame::Error("ERR syntax error".into());
                    }
                    *protocol = Protocol::Resp3;
                    hello_reply(*protocol, client_id)
                }
                _ => Frame::Error("NOPROTO unsupported protocol version".into()),
            },
        },
        _ => Frame::Error(format!("ERR unknown command '{name}'")),
    }
}

fn hello_reply(protocol: Protocol, client_id: u64) -> Frame {
    Frame::Map(vec![
        (
            Frame::Bulk(Bytes::from_static(b"server")),
            Frame::Bulk(Bytes::from_static(b"redis")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"version")),
            Frame::Bulk(Bytes::from_static(b"rocket-mem-0.1.0")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"proto")),
            Frame::Integer(match protocol {
                Protocol::Resp2 => 2,
                Protocol::Resp3 => 3,
            }),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"id")),
            Frame::Integer(client_id as i64),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"mode")),
            Frame::Bulk(Bytes::from_static(b"standalone")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"role")),
            Frame::Bulk(Bytes::from_static(b"master")),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"modules")),
            Frame::Array(vec![]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use engine::Engine;
    use protocol::codec::Protocol;
    use protocol::Frame;

    fn cmd(parts: &[&[u8]]) -> Frame {
        Frame::Array(
            parts
                .iter()
                .map(|p| Frame::Bulk(Bytes::copy_from_slice(p)))
                .collect(),
        )
    }

    #[test]
    fn dispatch_is_case_insensitive_on_command_name() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"set", b"k", b"v"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SeT", b"k2", b"v2"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
    }

    #[test]
    fn dispatch_set_then_get_round_trips() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"foo", b"bar"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"foo"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"bar"))
        );
    }

    #[test]
    fn dispatch_get_on_missing_key_returns_null() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"GET", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Null
        );
    }

    #[test]
    fn dispatch_wrongtype_is_mapped_to_a_resp_error_frame() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        // HSET on a string key: WRONGTYPE
        let reply = dispatch(
            &engine,
            cmd(&[b"HSET", b"k", b"f", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error(
                "WRONGTYPE Operation against a key holding the wrong kind of value".into()
            )
        );
    }

    #[test]
    fn dispatch_unknown_command_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"NOPE"]), &mut Protocol::default(), 1),
            Frame::Error("ERR unknown command 'NOPE'".into())
        );
    }

    #[test]
    fn dispatch_on_non_array_frame_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                Frame::Simple("not a command".into()),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR invalid request, expected array of bulk strings".into())
        );
    }

    #[test]
    fn dispatch_on_empty_array_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, Frame::Array(vec![]), &mut Protocol::default(), 1),
            Frame::Error("ERR empty command".into())
        );
    }

    #[test]
    fn ping_with_no_args_replies_pong() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"PING"]), &mut Protocol::default(), 1),
            Frame::Simple("PONG".into())
        );
    }

    #[test]
    fn ping_with_a_message_echoes_it_back_as_a_bulk_string() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"PING", b"hello"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"hello"))
        );
    }

    #[test]
    fn echo_returns_its_argument() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"ECHO", b"hi"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"hi"))
        );
    }

    #[test]
    fn select_always_replies_ok_single_db_only() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SELECT", b"0"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
    }

    #[test]
    fn command_replies_with_an_empty_array_rather_than_erroring() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"COMMAND"]), &mut Protocol::default(), 1),
            Frame::Array(vec![])
        );
    }

    #[test]
    fn info_replies_a_non_empty_bulk_string() {
        let engine = Engine::new();
        let Frame::Bulk(info) = dispatch(&engine, cmd(&[b"INFO"]), &mut Protocol::default(), 1)
        else {
            panic!("expected Bulk")
        };
        assert!(!info.is_empty());
    }

    #[test]
    fn set_with_too_few_args_returns_resp_error_not_a_panic() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SET", b"onlykey"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'set' command".into())
        );
    }

    #[test]
    fn hset_with_too_few_args_returns_resp_error_not_a_panic() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HSET", b"h", b"field"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'hset' command".into())
        );
    }

    #[test]
    fn echo_with_no_args_returns_resp_error_not_a_panic() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"ECHO"]), &mut Protocol::default(), 1),
            Frame::Error("ERR wrong number of arguments for 'echo' command".into())
        );
    }

    #[test]
    fn set_nx_returns_null_when_key_already_exists() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"old"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SET", b"k", b"new", b"NX"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Null
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"old"))
        );
    }

    #[test]
    fn set_with_ex_flag_returns_a_clear_not_implemented_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SET", b"k", b"v", b"EX", b"10"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR syntax error: EX/PX are not supported yet (planned Sprint 4)".into())
        );
    }

    #[test]
    fn incrby_parses_the_delta_argument() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"counter", b"10"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"INCRBY", b"counter", b"5"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(15)
        );
    }

    #[test]
    fn incrby_on_a_non_integer_delta_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"INCRBY", b"counter", b"notanumber"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR value is not an integer or out of range".into())
        );
    }

    #[test]
    fn hdel_hgetall_hexists_hlen_round_trip() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"HSET", b"h", b"f", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HEXISTS", b"h", b"f"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"HLEN", b"h"]), &mut Protocol::default(), 1),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HDEL", b"h", b"f"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HGETALL", b"h"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![])
        );
    }

    #[test]
    fn list_commands_round_trip() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"a"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"b"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"LPUSH", b"l", b"z"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"LLEN", b"l"]), &mut Protocol::default(), 1),
            Frame::Integer(3)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LRANGE", b"l", b"0", b"-1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"z")),
                Frame::Bulk(Bytes::from_static(b"a")),
                Frame::Bulk(Bytes::from_static(b"b")),
            ])
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"RPOP", b"l"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"b"))
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"LPOP", b"l"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"z"))
        );
    }

    #[test]
    fn set_type_commands_round_trip() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SADD", b"s", b"x"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SISMEMBER", b"s", b"x"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SISMEMBER", b"s", b"y"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(0)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"SCARD", b"s"]), &mut Protocol::default(), 1),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SREM", b"s", b"x"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SMEMBERS", b"s"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![])
        );
    }

    #[test]
    fn hello_with_no_args_reports_current_protocol_without_switching() {
        let engine = Engine::new();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch(&engine, cmd(&[b"HELLO"]), &mut protocol, 7);
        assert_eq!(protocol, Protocol::Resp2); // unchanged
        assert_eq!(
            reply,
            Frame::Map(vec![
                (
                    Frame::Bulk(Bytes::from_static(b"server")),
                    Frame::Bulk(Bytes::from_static(b"redis"))
                ),
                (
                    Frame::Bulk(Bytes::from_static(b"version")),
                    Frame::Bulk(Bytes::from_static(b"rocket-mem-0.1.0"))
                ),
                (Frame::Bulk(Bytes::from_static(b"proto")), Frame::Integer(2)),
                (Frame::Bulk(Bytes::from_static(b"id")), Frame::Integer(7)),
                (
                    Frame::Bulk(Bytes::from_static(b"mode")),
                    Frame::Bulk(Bytes::from_static(b"standalone"))
                ),
                (
                    Frame::Bulk(Bytes::from_static(b"role")),
                    Frame::Bulk(Bytes::from_static(b"master"))
                ),
                (
                    Frame::Bulk(Bytes::from_static(b"modules")),
                    Frame::Array(vec![])
                ),
            ])
        );
    }

    #[test]
    fn hello_2_switches_protocol_to_resp2() {
        let engine = Engine::new();
        let mut protocol = Protocol::Resp3;
        let reply = dispatch(&engine, cmd(&[b"HELLO", b"2"]), &mut protocol, 1);
        assert_eq!(protocol, Protocol::Resp2);
        let Frame::Map(pairs) = reply else {
            panic!("expected Map")
        };
        assert!(pairs.contains(&(Frame::Bulk(Bytes::from_static(b"proto")), Frame::Integer(2))));
    }

    #[test]
    fn hello_3_switches_protocol_to_resp3() {
        let engine = Engine::new();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch(&engine, cmd(&[b"HELLO", b"3"]), &mut protocol, 42);
        assert_eq!(protocol, Protocol::Resp3);
        assert_eq!(
            reply,
            Frame::Map(vec![
                (
                    Frame::Bulk(Bytes::from_static(b"server")),
                    Frame::Bulk(Bytes::from_static(b"redis"))
                ),
                (
                    Frame::Bulk(Bytes::from_static(b"version")),
                    Frame::Bulk(Bytes::from_static(b"rocket-mem-0.1.0"))
                ),
                (Frame::Bulk(Bytes::from_static(b"proto")), Frame::Integer(3)),
                (Frame::Bulk(Bytes::from_static(b"id")), Frame::Integer(42)),
                (
                    Frame::Bulk(Bytes::from_static(b"mode")),
                    Frame::Bulk(Bytes::from_static(b"standalone"))
                ),
                (
                    Frame::Bulk(Bytes::from_static(b"role")),
                    Frame::Bulk(Bytes::from_static(b"master"))
                ),
                (
                    Frame::Bulk(Bytes::from_static(b"modules")),
                    Frame::Array(vec![])
                ),
            ])
        );
    }

    #[test]
    fn hello_with_unsupported_protover_returns_noproto_and_leaves_protocol_unchanged() {
        let engine = Engine::new();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch(&engine, cmd(&[b"HELLO", b"4"]), &mut protocol, 1);
        assert_eq!(protocol, Protocol::Resp2); // unchanged
        assert_eq!(
            reply,
            Frame::Error("NOPROTO unsupported protocol version".into())
        );
    }

    #[test]
    fn hello_with_extra_args_after_protover_is_a_syntax_error() {
        let engine = Engine::new();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch(
            &engine,
            cmd(&[b"HELLO", b"3", b"AUTH", b"user", b"pass"]),
            &mut protocol,
            1,
        );
        assert_eq!(protocol, Protocol::Resp2); // unchanged — the switch never happened
        assert_eq!(reply, Frame::Error("ERR syntax error".into()));
    }
}
