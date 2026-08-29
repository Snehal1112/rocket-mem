use bytes::Bytes;
use engine::{commands, Engine, Value};
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

pub fn dispatch(engine: &Engine, frame: Frame) -> Frame {
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
        "GET" => match commands::string::get(engine, &rest[0]) {
            Ok(Some(b)) => Frame::Bulk(b),
            Ok(None) => Frame::Null,
            Err(e) => engine_error_to_frame(e),
        },
        "SET" => {
            engine.set(rest[0].clone(), Value::String(rest[1].clone()));
            Frame::Simple("OK".into())
        }
        "APPEND" => match commands::string::append(engine, rest[0].clone(), &rest[1]) {
            Ok(len) => Frame::Integer(len as i64),
            Err(e) => engine_error_to_frame(e),
        },
        "STRLEN" => match commands::string::strlen(engine, &rest[0]) {
            Ok(len) => Frame::Integer(len as i64),
            Err(e) => engine_error_to_frame(e),
        },
        "INCR" => match commands::string::incr_by(engine, rest[0].clone(), 1) {
            Ok(n) => Frame::Integer(n),
            Err(e) => engine_error_to_frame(e),
        },
        "DECR" => match commands::string::incr_by(engine, rest[0].clone(), -1) {
            Ok(n) => Frame::Integer(n),
            Err(e) => engine_error_to_frame(e),
        },
        "HSET" => {
            match commands::hash::hset(engine, rest[0].clone(), rest[1].clone(), rest[2].clone()) {
                Ok(()) => Frame::Integer(1),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HGET" => match commands::hash::hget(engine, &rest[0], &rest[1]) {
            Ok(Some(b)) => Frame::Bulk(b),
            Ok(None) => Frame::Null,
            Err(e) => engine_error_to_frame(e),
        },
        _ => Frame::Error(format!("ERR unknown command '{name}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use engine::Engine;
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
            dispatch(&engine, cmd(&[b"set", b"k", b"v"])),
            Frame::Simple("OK".into())
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"SeT", b"k2", b"v2"])),
            Frame::Simple("OK".into())
        );
    }

    #[test]
    fn dispatch_set_then_get_round_trips() {
        let engine = Engine::new();
        dispatch(&engine, cmd(&[b"SET", b"foo", b"bar"]));
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"foo"])),
            Frame::Bulk(Bytes::from_static(b"bar"))
        );
    }

    #[test]
    fn dispatch_get_on_missing_key_returns_null() {
        let engine = Engine::new();
        assert_eq!(dispatch(&engine, cmd(&[b"GET", b"missing"])), Frame::Null);
    }

    #[test]
    fn dispatch_wrongtype_is_mapped_to_a_resp_error_frame() {
        let engine = Engine::new();
        dispatch(&engine, cmd(&[b"SET", b"k", b"v"]));
        // HSET on a string key: WRONGTYPE
        let reply = dispatch(&engine, cmd(&[b"HSET", b"k", b"f", b"v"]));
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
            dispatch(&engine, cmd(&[b"NOPE"])),
            Frame::Error("ERR unknown command 'NOPE'".into())
        );
    }

    #[test]
    fn dispatch_on_non_array_frame_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, Frame::Simple("not a command".into())),
            Frame::Error("ERR invalid request, expected array of bulk strings".into())
        );
    }

    #[test]
    fn dispatch_on_empty_array_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, Frame::Array(vec![])),
            Frame::Error("ERR empty command".into())
        );
    }
}
