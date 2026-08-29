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

fn format_score(score: f64) -> Bytes {
    if score.fract() == 0.0 && score.is_finite() {
        Bytes::from((score as i64).to_string())
    } else {
        Bytes::from(score.to_string())
    }
}

/// `EXPIREAT`/`PEXPIREAT` give an absolute Unix timestamp; `Instant` has no epoch relationship,
/// so the absolute target is first resolved via `SystemTime`, then re-expressed as a delta
/// applied to `Instant::now()`. A target already in the past collapses to `Duration::ZERO`,
/// which the very next expiry check reads as already-expired — see
/// ../../specs/2026-08-30-sprint-4-spec.md for why this two-step conversion is necessary.
fn instant_from_unix_ms(target_unix_ms: i64) -> std::time::Instant {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let target = UNIX_EPOCH + Duration::from_millis(target_unix_ms.max(0) as u64);
    let delta = target
        .duration_since(SystemTime::now())
        .unwrap_or(Duration::ZERO);
    std::time::Instant::now() + delta
}

fn parse_score(raw: &[u8]) -> Result<f64, Frame> {
    let score: f64 = std::str::from_utf8(raw)
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| Frame::Error("ERR value is not a valid float".into()))?;
    if !score.is_finite() {
        return Err(Frame::Error("ERR value is not a valid float".into()));
    }
    Ok(score)
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
        "DEL" => {
            require_args!(rest, 1, "del");
            let deleted = rest.iter().filter(|k| engine.del(k)).count();
            Frame::Integer(deleted as i64)
        }
        "EXISTS" => {
            require_args!(rest, 1, "exists");
            let count = rest.iter().filter(|k| engine.exists(k)).count();
            Frame::Integer(count as i64)
        }
        "SET" => {
            require_args!(rest, 2, "set");
            let key = rest[0].clone();
            let val = rest[1].clone();
            let flags: Vec<String> = rest[2..]
                .iter()
                .map(|b| String::from_utf8_lossy(b).to_ascii_uppercase())
                .collect();
            let ex_ms: Option<i64> = if let Some(pos) = flags.iter().position(|f| f == "EX") {
                match rest
                    .get(2 + pos + 1)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    Some(secs) => Some(secs.saturating_mul(1000)),
                    None => {
                        return Frame::Error("ERR value is not an integer or out of range".into())
                    }
                }
            } else if let Some(pos) = flags.iter().position(|f| f == "PX") {
                match rest
                    .get(2 + pos + 1)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    Some(ms) => Some(ms),
                    None => {
                        return Frame::Error("ERR value is not an integer or out of range".into())
                    }
                }
            } else {
                None
            };

            let applied = if flags.iter().any(|f| f == "NX") {
                commands::string::set_nx(engine, key.clone(), val)
            } else if flags.iter().any(|f| f == "XX") {
                commands::string::set_xx(engine, key.clone(), val)
            } else {
                engine.set(key.clone(), Value::String(val));
                true
            };
            if !applied {
                return Frame::Null;
            }
            if let Some(ms) = ex_ms {
                engine.expire_at(
                    &key,
                    std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0) as u64),
                );
            }
            Frame::Simple("OK".into())
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
        "GETRANGE" => {
            require_args!(rest, 3, "getrange");
            let (start, end) = match (
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
            match commands::string::getrange(engine, &rest[0], start, end) {
                Ok(b) => Frame::Bulk(b),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SETRANGE" => {
            require_args!(rest, 3, "setrange");
            let offset: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            if offset < 0 {
                return Frame::Error("ERR offset is out of range".into());
            }
            match commands::string::setrange(engine, rest[0].clone(), offset as usize, &rest[2]) {
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
            let pairs = &rest[1..];
            if pairs.len() % 2 != 0 {
                return Frame::Error("ERR wrong number of arguments for 'hset' command".into());
            }
            let mut added = 0i64;
            for pair in pairs.chunks_exact(2) {
                match commands::hash::hset(
                    engine,
                    rest[0].clone(),
                    pair[0].clone(),
                    pair[1].clone(),
                ) {
                    Ok(true) => added += 1,
                    Ok(false) => {}
                    Err(e) => return engine_error_to_frame(e),
                }
            }
            Frame::Integer(added)
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
        "HSCAN" => {
            require_args!(rest, 2, "hscan");
            // No MATCH/COUNT/NOVALUES support yet, matching the keyspace SCAN's current scope.
            // A hash already lives fully in memory (HGETALL reads it all in one shot), so unlike
            // keyspace SCAN there's no chunking to design here -- one call always returns
            // everything and reports cursor "0" (done), which is a legitimate SCAN-family reply.
            if std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .is_none()
            {
                return Frame::Error("ERR invalid cursor".into());
            }
            match commands::hash::hgetall(engine, &rest[0]) {
                Ok(map) => Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"0")),
                    Frame::Array(
                        map.into_iter()
                            .flat_map(|(f, v)| [Frame::Bulk(f), Frame::Bulk(v)])
                            .collect(),
                    ),
                ]),
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
        "HINCRBY" => {
            require_args!(rest, 3, "hincrby");
            let delta: i64 = match std::str::from_utf8(&rest[2])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            match commands::hash::hincrby(engine, rest[0].clone(), rest[1].clone(), delta) {
                Ok(n) => Frame::Integer(n),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HKEYS" => {
            require_args!(rest, 1, "hkeys");
            match commands::hash::hkeys(engine, &rest[0]) {
                Ok(fields) => Frame::Array(fields.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HVALS" => {
            require_args!(rest, 1, "hvals");
            match commands::hash::hvals(engine, &rest[0]) {
                Ok(vals) => Frame::Array(vals.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HMGET" => {
            require_args!(rest, 2, "hmget");
            match commands::hash::hmget(engine, &rest[0], &rest[1..]) {
                Ok(vals) => Frame::Array(
                    vals.into_iter()
                        .map(|v| match v {
                            Some(b) => Frame::Bulk(b),
                            None => Frame::Null,
                        })
                        .collect(),
                ),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "HSETNX" => {
            require_args!(rest, 3, "hsetnx");
            match commands::hash::hsetnx(engine, rest[0].clone(), rest[1].clone(), rest[2].clone())
            {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "RPUSH" => {
            require_args!(rest, 2, "rpush");
            for val in &rest[1..] {
                if let Err(e) = commands::list::rpush(engine, rest[0].clone(), val.clone()) {
                    return engine_error_to_frame(e);
                }
            }
            match commands::list::llen(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LPUSH" => {
            require_args!(rest, 2, "lpush");
            for val in &rest[1..] {
                if let Err(e) = commands::list::lpush(engine, rest[0].clone(), val.clone()) {
                    return engine_error_to_frame(e);
                }
            }
            match commands::list::llen(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
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
        "LINDEX" => {
            require_args!(rest, 2, "lindex");
            let index: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            match commands::list::lindex(engine, &rest[0], index) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LSET" => {
            require_args!(rest, 3, "lset");
            let index: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            match commands::list::lset(engine, rest[0].clone(), index, rest[2].clone()) {
                Ok(true) => Frame::Simple("OK".into()),
                Ok(false) => Frame::Error("ERR index out of range".into()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LTRIM" => {
            require_args!(rest, 3, "ltrim");
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
            match commands::list::ltrim(engine, rest[0].clone(), start, stop) {
                Ok(()) => Frame::Simple("OK".into()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LREM" => {
            require_args!(rest, 3, "lrem");
            let count: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            match commands::list::lrem(engine, rest[0].clone(), count, &rest[2]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LINSERT" => {
            require_args!(rest, 4, "linsert");
            let before = match String::from_utf8_lossy(&rest[1])
                .to_ascii_uppercase()
                .as_str()
            {
                "BEFORE" => true,
                "AFTER" => false,
                _ => return Frame::Error("ERR syntax error".into()),
            };
            match commands::list::linsert(
                engine,
                rest[0].clone(),
                before,
                &rest[2],
                rest[3].clone(),
            ) {
                Ok(n) => Frame::Integer(n),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SADD" => {
            require_args!(rest, 2, "sadd");
            let mut added = 0i64;
            for member in &rest[1..] {
                match commands::set::sadd(engine, rest[0].clone(), member.clone()) {
                    Ok(true) => added += 1,
                    Ok(false) => {}
                    Err(e) => return engine_error_to_frame(e),
                }
            }
            Frame::Integer(added)
        }
        "SREM" => {
            require_args!(rest, 2, "srem");
            let mut removed = 0i64;
            for member in &rest[1..] {
                match commands::set::srem(engine, &rest[0], member) {
                    Ok(true) => removed += 1,
                    Ok(false) => {}
                    Err(e) => return engine_error_to_frame(e),
                }
            }
            Frame::Integer(removed)
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
        "SINTER" => {
            require_args!(rest, 1, "sinter");
            match commands::set::sinter(engine, rest) {
                Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SUNION" => {
            require_args!(rest, 1, "sunion");
            match commands::set::sunion(engine, rest) {
                Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SDIFF" => {
            require_args!(rest, 1, "sdiff");
            match commands::set::sdiff(engine, rest) {
                Ok(members) => Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SINTERSTORE" => {
            require_args!(rest, 2, "sinterstore");
            match commands::set::sinterstore(engine, rest[0].clone(), &rest[1..]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SUNIONSTORE" => {
            require_args!(rest, 2, "sunionstore");
            match commands::set::sunionstore(engine, rest[0].clone(), &rest[1..]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SDIFFSTORE" => {
            require_args!(rest, 2, "sdiffstore");
            match commands::set::sdiffstore(engine, rest[0].clone(), &rest[1..]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SPOP" => {
            require_args!(rest, 1, "spop");
            match commands::set::spop(engine, &rest[0]) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SRANDMEMBER" => {
            require_args!(rest, 1, "srandmember");
            match commands::set::srandmember(engine, &rest[0]) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "GETSET" => {
            require_args!(rest, 2, "getset");
            match commands::string::getset(engine, rest[0].clone(), rest[1].clone()) {
                Ok(Some(b)) => Frame::Bulk(b),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "MSET" => {
            require_args!(rest, 2, "mset");
            if rest.len() % 2 != 0 {
                return Frame::Error("ERR wrong number of arguments for 'mset' command".into());
            }
            let pairs: Vec<(Bytes, Bytes)> = rest
                .chunks(2)
                .map(|c| (c[0].clone(), c[1].clone()))
                .collect();
            commands::string::mset(engine, pairs);
            Frame::Simple("OK".into())
        }
        "MGET" => {
            require_args!(rest, 1, "mget");
            let vals = commands::string::mget(engine, rest);
            Frame::Array(
                vals.into_iter()
                    .map(|v| match v {
                        Some(b) => Frame::Bulk(b),
                        None => Frame::Null,
                    })
                    .collect(),
            )
        }
        "MSETNX" => {
            require_args!(rest, 2, "msetnx");
            if rest.len() % 2 != 0 {
                return Frame::Error("ERR wrong number of arguments for 'msetnx' command".into());
            }
            let pairs: Vec<(Bytes, Bytes)> = rest
                .chunks(2)
                .map(|c| (c[0].clone(), c[1].clone()))
                .collect();
            match commands::string::msetnx(engine, pairs) {
                true => Frame::Integer(1),
                false => Frame::Integer(0),
            }
        }
        "RENAME" => {
            require_args!(rest, 2, "rename");
            match commands::keys::rename(engine, &rest[0], rest[1].clone()) {
                Ok(()) => Frame::Simple("OK".into()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "RENAMENX" => {
            require_args!(rest, 2, "renamenx");
            match commands::keys::renamenx(engine, &rest[0], rest[1].clone()) {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "TYPE" => {
            require_args!(rest, 1, "type");
            Frame::Simple(commands::keys::key_type(engine, &rest[0]).into())
        }
        "ZADD" => {
            require_args!(rest, 3, "zadd");
            let score = match parse_score(&rest[1]) {
                Ok(s) => s,
                Err(e) => return e,
            };
            match commands::sorted_set::zadd(engine, rest[0].clone(), score, rest[2].clone()) {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "ZSCORE" => {
            require_args!(rest, 2, "zscore");
            match commands::sorted_set::zscore(engine, &rest[0], &rest[1]) {
                Ok(Some(score)) => Frame::Bulk(format_score(score)),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "ZREM" => {
            require_args!(rest, 2, "zrem");
            match commands::sorted_set::zrem(engine, &rest[0], &rest[1]) {
                Ok(true) => Frame::Integer(1),
                Ok(false) => Frame::Integer(0),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "ZCARD" => {
            require_args!(rest, 1, "zcard");
            match commands::sorted_set::zcard(engine, &rest[0]) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "ZINCRBY" => {
            require_args!(rest, 3, "zincrby");
            let delta = match parse_score(&rest[1]) {
                Ok(s) => s,
                Err(e) => return e,
            };
            match commands::sorted_set::zincrby(engine, rest[0].clone(), delta, rest[2].clone()) {
                Ok(score) => Frame::Bulk(format_score(score)),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "ZRANGE" => {
            require_args!(rest, 3, "zrange");
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
            match commands::sorted_set::zrange(engine, &rest[0], start, stop) {
                Ok(items) => Frame::Array(items.into_iter().map(Frame::Bulk).collect()),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "ZRANK" => {
            require_args!(rest, 2, "zrank");
            match commands::sorted_set::zrank(engine, &rest[0], &rest[1]) {
                Ok(Some(r)) => Frame::Integer(r as i64),
                Ok(None) => Frame::Null,
                Err(e) => engine_error_to_frame(e),
            }
        }
        "KEYS" => {
            require_args!(rest, 1, "keys");
            let pattern = &rest[0];
            Frame::Array(
                engine
                    .keys()
                    .into_iter()
                    .filter(|k| engine::glob::glob_match(pattern, k))
                    .map(Frame::Bulk)
                    .collect(),
            )
        }
        "SCAN" => {
            require_args!(rest, 1, "scan");
            let cursor: u64 = match std::str::from_utf8(&rest[0])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR invalid cursor".into()),
            };
            let (next, keys) = engine.scan(cursor);
            Frame::Array(vec![
                Frame::Bulk(Bytes::from(next.to_string())),
                Frame::Array(keys.into_iter().map(Frame::Bulk).collect()),
            ])
        }
        "RANDOMKEY" => match commands::keys::randomkey(engine) {
            Some(k) => Frame::Bulk(k),
            None => Frame::Null,
        },
        "EXPIRE" | "PEXPIRE" => {
            require_args!(rest, 2, name.to_ascii_lowercase().as_str());
            let n: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            let delta = if name == "EXPIRE" {
                std::time::Duration::from_secs(n.max(0) as u64)
            } else {
                std::time::Duration::from_millis(n.max(0) as u64)
            };
            match engine.expire_at(&rest[0], std::time::Instant::now() + delta) {
                true => Frame::Integer(1),
                false => Frame::Integer(0),
            }
        }
        "EXPIREAT" | "PEXPIREAT" => {
            require_args!(rest, 2, name.to_ascii_lowercase().as_str());
            let n: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            let target_unix_ms = if name == "EXPIREAT" {
                n.saturating_mul(1000)
            } else {
                n
            };
            match engine.expire_at(&rest[0], instant_from_unix_ms(target_unix_ms)) {
                true => Frame::Integer(1),
                false => Frame::Integer(0),
            }
        }
        "TTL" => {
            require_args!(rest, 1, "ttl");
            match engine.ttl(&rest[0]) {
                engine::TtlStatus::NoSuchKey => Frame::Integer(-2),
                engine::TtlStatus::NoExpiry => Frame::Integer(-1),
                engine::TtlStatus::Remaining(d) => Frame::Integer(d.as_secs().max(1) as i64),
            }
        }
        "PTTL" => {
            require_args!(rest, 1, "pttl");
            match engine.ttl(&rest[0]) {
                engine::TtlStatus::NoSuchKey => Frame::Integer(-2),
                engine::TtlStatus::NoExpiry => Frame::Integer(-1),
                engine::TtlStatus::Remaining(d) => Frame::Integer(d.as_millis().max(1) as i64),
            }
        }
        "PERSIST" => {
            require_args!(rest, 1, "persist");
            match engine.persist(&rest[0]) {
                true => Frame::Integer(1),
                false => Frame::Integer(0),
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

/// Wraps `dispatch`, additionally appending successful write commands to `aof`. `dispatch`
/// itself is never modified — see ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md
/// for why AOF logging lives here instead of inside dispatch's own match arms.
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    let original_frame = frame.clone();
    let reply = dispatch(engine, frame, protocol, client_id);
    if let Frame::Error(_) = reply {
        return reply;
    }

    let Frame::Array(items) = &original_frame else {
        return reply;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return reply;
    };
    let name = String::from_utf8_lossy(name_bytes).to_ascii_uppercase();
    if !crate::aof::WRITE_COMMANDS.contains(&name.as_str()) {
        return reply;
    }

    // A Vec, not an Option: `SET k v EX n` logs as *two* frames (the flagless SET plus an
    // absolute PEXPIREAT), and several cases log none at all.
    let to_log: Vec<Frame> = match name.as_str() {
        "SPOP" => match (&reply, items.get(1)) {
            (Frame::Bulk(member), Some(key)) => vec![Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SREM")),
                key.clone(),
                Frame::Bulk(member.clone()),
            ])],
            _ => Vec::new(), // Frame::Null — nothing was popped
        },
        "EXPIRE" | "PEXPIRE" | "EXPIREAT" | "PEXPIREAT" => match &reply {
            Frame::Integer(1) => rewrite_expire_family_to_pexpireat(items)
                .map(|f| vec![f])
                .unwrap_or_default(),
            _ => Vec::new(), // Frame::Integer(0) — the key didn't exist, nothing changed
        },
        "SET" => match &reply {
            // Simple("OK") means the write applied, so any EX/PX on it needs the same
            // relative→absolute rewrite the EXPIRE family gets. A Null reply is an NX/XX
            // no-op: logging it verbatim is safe (replay re-resolves the condition the same
            // way and applies nothing, TTL included), so it needs no rewrite.
            Frame::Simple(_) => {
                rewrite_set_ttl_to_pexpireat(items).unwrap_or_else(|| vec![original_frame.clone()])
            }
            _ => vec![original_frame.clone()],
        },
        _ => vec![original_frame.clone()],
    };

    // Note: to_log may contain two frames (e.g., for SET ... EX/PX: [flagless SET, PEXPIREAT]).
    // Each frame is appended separately. Under FsyncPolicy::Always, each append() call fsyncs
    // independently, so a crash between appends would durably record the SET without its
    // PEXPIREAT — on replay the key would live forever, silently dropping the TTL. Full
    // crash/corrupt-tail recovery semantics are scoped to Task 06 (06-aof-replay-and-corrupt-recovery.md).
    for frame_to_log in to_log {
        // a logging failure must not fail the client's reply
        let _ = aof.append(frame_to_log);
        // fsync timing for Always lives inside AofWriter::append itself; EverySecond's
        // periodic fsync loop lives in this plan's Task 2 (connection.rs); Never does
        // nothing here.
    }
    reply
}

/// Rewrites a logged EXPIRE/PEXPIRE/EXPIREAT/PEXPIREAT command's args into an absolute
/// `PEXPIREAT key <unix-ms>`, computed independently via SystemTime (not the Instant already
/// used inside `dispatch`'s own EXPIRE arm) — see
/// ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md's note on this small, accepted
/// duplication.
fn rewrite_expire_family_to_pexpireat(items: &[Frame]) -> Option<Frame> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Frame::Bulk(name) = items.first()? else {
        return None;
    };
    let Frame::Bulk(key) = items.get(1)? else {
        return None;
    };
    let Frame::Bulk(arg) = items.get(2)? else {
        return None;
    };
    let n: i64 = std::str::from_utf8(arg).ok()?.parse().ok()?;
    let name_upper = String::from_utf8_lossy(name).to_ascii_uppercase();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    let target_unix_ms = match name_upper.as_str() {
        "EXPIRE" => now_ms + n.saturating_mul(1000),
        "PEXPIRE" => now_ms + n,
        "EXPIREAT" => n.saturating_mul(1000),
        "PEXPIREAT" => n,
        _ => return None,
    };
    Some(Frame::Array(vec![
        Frame::Bulk(Bytes::from_static(b"PEXPIREAT")),
        Frame::Bulk(key.clone()),
        Frame::Bulk(Bytes::from(target_unix_ms.to_string())),
    ]))
}

/// `SET key val EX n` / `PX n` (from 03-expire-family-and-set-ttl-dispatcher.md) carries a
/// *relative* TTL, so logging it verbatim restarts the countdown from replay time — the same
/// drift the EXPIRE family is rewritten to avoid, and the reason a static "everything else is
/// deterministic" rule isn't quite enough for SET. Splits the command into the flagless SET
/// (every other flag, e.g. NX/XX, preserved in place) plus an absolute `PEXPIREAT`.
/// Returns `None` when there was no EX/PX at all — nothing to rewrite, log it verbatim.
/// When both EX and PX are present, EX takes precedence, matching dispatch's SET arm behavior.
fn rewrite_set_ttl_to_pexpireat(items: &[Frame]) -> Option<Vec<Frame>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    if items.len() < 3 {
        return None; // SET k v is the shortest valid form; anything shorter never applied
    }
    let Frame::Bulk(key) = items.get(1)? else {
        return None;
    };
    // items = [SET, key, value, flags...] — only index 3 onward is the flag region.
    let mut kept: Vec<Frame> = items[..3].to_vec();
    let mut ttl_ms: Option<i64> = None;

    // First pass: check for EX, which takes precedence over PX (matches dispatch's SET arm).
    let mut i = 3;
    while i < items.len() {
        let Frame::Bulk(raw) = &items[i] else {
            i += 1;
            continue;
        };
        let flag = String::from_utf8_lossy(raw).to_ascii_uppercase();
        if flag == "EX" {
            let Some(Frame::Bulk(v)) = items.get(i + 1) else {
                return None; // malformed; dispatch already rejected it, so log verbatim
            };
            let n: i64 = std::str::from_utf8(v).ok()?.parse().ok()?;
            ttl_ms = Some(n.saturating_mul(1000));
            break; // EX found; stop searching (it takes precedence over any later PX)
        }
        i += 1;
    }

    // Second pass: if no EX found, check for PX.
    if ttl_ms.is_none() {
        let mut i = 3;
        while i < items.len() {
            let Frame::Bulk(raw) = &items[i] else {
                i += 1;
                continue;
            };
            let flag = String::from_utf8_lossy(raw).to_ascii_uppercase();
            if flag == "PX" {
                let Some(Frame::Bulk(v)) = items.get(i + 1) else {
                    return None; // malformed; dispatch already rejected it, so log verbatim
                };
                let n: i64 = std::str::from_utf8(v).ok()?.parse().ok()?;
                ttl_ms = Some(n);
                break; // PX found; stop searching
            }
            i += 1;
        }
    }

    // Third pass: build the kept (non-TTL-flag) version of the command.
    let mut i = 3;
    while i < items.len() {
        let Frame::Bulk(raw) = &items[i] else {
            kept.push(items[i].clone());
            i += 1;
            continue;
        };
        let flag = String::from_utf8_lossy(raw).to_ascii_uppercase();
        if flag == "EX" || flag == "PX" {
            i += 2; // skip both the flag and its value
        } else {
            kept.push(items[i].clone());
            i += 1;
        }
    }

    let ttl_ms = ttl_ms?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(vec![
        Frame::Array(kept),
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"PEXPIREAT")),
            Frame::Bulk(key.clone()),
            Frame::Bulk(Bytes::from((now_ms + ttl_ms).to_string())),
        ]),
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
    fn exists_counts_keys_that_exist_including_duplicates() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"a", b"1"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"EXISTS", b"a", b"a", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(2)
        );
    }

    #[test]
    fn del_removes_keys_and_returns_the_count_actually_deleted() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"a", b"1"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"SET", b"b", b"2"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"DEL", b"a", b"b", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(2)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"a"]), &mut Protocol::default(), 1),
            Frame::Null
        );
    }

    #[test]
    fn getrange_returns_the_inclusive_byte_slice_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"Hello World"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"GETRANGE", b"k", b"0", b"4"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"Hello"))
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"GETRANGE", b"k", b"-5", b"-1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"World"))
        );
    }

    #[test]
    fn getrange_on_a_non_integer_index_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"GETRANGE", b"k", b"notanumber", b"-1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR value is not an integer or out of range".into())
        );
    }

    #[test]
    fn setrange_overwrites_and_reports_the_new_length_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"Hello World"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SETRANGE", b"k", b"6", b"Redis!"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(12)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"Hello Redis!"))
        );
    }

    #[test]
    fn setrange_with_a_negative_offset_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SETRANGE", b"k", b"-1", b"x"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR offset is out of range".into())
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
    fn set_with_ex_applies_a_relative_ttl_in_seconds() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v", b"EX", b"100"]),
            &mut Protocol::default(),
            1,
        );
        let Frame::Integer(secs) =
            dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1)
        else {
            panic!("expected Integer")
        };
        assert!((1..=100).contains(&secs));
    }

    #[test]
    fn set_with_px_applies_a_relative_ttl_in_milliseconds() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v", b"PX", b"60000"]),
            &mut Protocol::default(),
            1,
        );
        let Frame::Integer(ms) =
            dispatch(&engine, cmd(&[b"PTTL", b"k"]), &mut Protocol::default(), 1)
        else {
            panic!("expected Integer")
        };
        assert!((1..=60000).contains(&ms));
    }

    #[test]
    fn set_without_ex_or_px_leaves_no_ttl() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
            Frame::Integer(-1)
        );
    }

    #[test]
    fn set_with_a_non_integer_ex_value_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SET", b"k", b"v", b"EX", b"soon"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR value is not an integer or out of range".into())
        );
    }

    #[test]
    fn set_overwriting_an_existing_key_with_a_ttl_clears_the_old_ttl() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"old", b"EX", b"100"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"new"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
            Frame::Integer(-1)
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
    fn rpush_with_multiple_values_pushes_all_in_order_and_returns_final_length() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"RPUSH", b"l", b"a", b"b", b"c"]),
                &mut Protocol::default(),
                1
            ),
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
                Frame::Bulk(Bytes::from_static(b"a")),
                Frame::Bulk(Bytes::from_static(b"b")),
                Frame::Bulk(Bytes::from_static(b"c")),
            ])
        );
    }

    #[test]
    fn lpush_with_multiple_values_prepends_each_so_the_last_argument_ends_up_first() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LPUSH", b"l", b"a", b"b", b"c"]),
                &mut Protocol::default(),
                1
            ),
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
                Frame::Bulk(Bytes::from_static(b"c")),
                Frame::Bulk(Bytes::from_static(b"b")),
                Frame::Bulk(Bytes::from_static(b"a")),
            ])
        );
    }

    #[test]
    fn sadd_with_multiple_members_returns_the_count_newly_added_not_total_args() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SADD", b"s", b"x"]),
            &mut Protocol::default(),
            1,
        );
        // "x" is already a member, so only "y" and "z" count as newly added.
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SADD", b"s", b"x", b"y", b"z"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(2)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"SCARD", b"s"]), &mut Protocol::default(), 1),
            Frame::Integer(3)
        );
    }

    #[test]
    fn srem_with_multiple_members_returns_the_count_actually_removed() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SADD", b"s", b"x", b"y"]),
            &mut Protocol::default(),
            1,
        );
        // "z" was never a member, so only "x" and "y" count as removed.
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SREM", b"s", b"x", b"y", b"z"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(2)
        );
    }

    #[test]
    fn hset_with_multiple_pairs_returns_the_count_of_new_fields() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"HSET", b"h", b"a", b"1"]),
            &mut Protocol::default(),
            1,
        );
        // "a" already exists (overwritten, not counted); "b" and "c" are new.
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HSET", b"h", b"a", b"99", b"b", b"2", b"c", b"3"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(2)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HGET", b"h", b"a"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"99"))
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"HLEN", b"h"]), &mut Protocol::default(), 1),
            Frame::Integer(3)
        );
    }

    #[test]
    fn hset_with_an_odd_number_of_field_value_args_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HSET", b"h", b"a", b"1", b"b"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'hset' command".into())
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
    fn getset_round_trips_through_dispatch() {
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
                cmd(&[b"GETSET", b"k", b"new"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"old"))
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"new"))
        );
    }

    #[test]
    fn mset_then_mget_round_trips_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"MSET", b"a", b"1", b"b", b"2"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"MGET", b"a", b"b", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"1")),
                Frame::Bulk(Bytes::from_static(b"2")),
                Frame::Null,
            ])
        );
    }

    #[test]
    fn mset_with_an_odd_number_of_args_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"MSET", b"a", b"1", b"b"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'mset' command".into())
        );
    }

    #[test]
    fn msetnx_returns_zero_when_a_key_already_exists() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"a", b"1"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"MSETNX", b"a", b"2", b"b", b"2"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(0)
        );
    }

    #[test]
    fn rename_then_get_round_trips_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"src", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"RENAME", b"src", b"dst"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"dst"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"v"))
        );
    }

    #[test]
    fn rename_on_missing_key_returns_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"RENAME", b"missing", b"dst"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("no such key".into())
        );
    }

    #[test]
    fn type_reports_none_for_a_missing_key() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"TYPE", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("none".into())
        );
    }

    #[test]
    fn randomkey_on_empty_keyspace_returns_null() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"RANDOMKEY"]), &mut Protocol::default(), 1),
            Frame::Null
        );
    }

    #[test]
    fn keys_returns_only_matching_keys() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"user:1", b"a"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"SET", b"user:2", b"b"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"SET", b"session:1", b"c"]),
            &mut Protocol::default(),
            1,
        );
        let Frame::Array(mut items) = dispatch(
            &engine,
            cmd(&[b"KEYS", b"user:*"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        items.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
        assert_eq!(
            items,
            vec![
                Frame::Bulk(Bytes::from_static(b"user:1")),
                Frame::Bulk(Bytes::from_static(b"user:2")),
            ]
        );
    }

    #[test]
    fn keys_on_empty_keyspace_returns_empty_array() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(&engine, cmd(&[b"KEYS", b"*"]), &mut Protocol::default(), 1),
            Frame::Array(vec![])
        );
    }

    #[test]
    fn scan_zero_returns_an_array_of_cursor_and_keys() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let reply = dispatch(&engine, cmd(&[b"SCAN", b"0"]), &mut Protocol::default(), 1);
        let Frame::Array(parts) = reply else {
            panic!("expected Array")
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], Frame::Bulk(Bytes::from_static(b"1")));
    }

    #[test]
    fn scan_with_a_non_numeric_cursor_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SCAN", b"notacursor"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR invalid cursor".into())
        );
    }

    #[test]
    fn hscan_returns_all_fields_in_one_call_with_a_done_cursor() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"HSET", b"h", b"a", b"1", b"b", b"2"]),
            &mut Protocol::default(),
            1,
        );
        let reply = dispatch(
            &engine,
            cmd(&[b"HSCAN", b"h", b"0"]),
            &mut Protocol::default(),
            1,
        );
        let Frame::Array(parts) = reply else {
            panic!("expected Array")
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], Frame::Bulk(Bytes::from_static(b"0")));
        let Frame::Array(pairs) = &parts[1] else {
            panic!("expected Array of field/value pairs")
        };
        assert_eq!(pairs.len(), 4); // 2 fields, flattened as field,value,field,value
    }

    #[test]
    fn hscan_on_missing_key_returns_an_empty_array_not_an_error() {
        let engine = Engine::new();
        let reply = dispatch(
            &engine,
            cmd(&[b"HSCAN", b"missing", b"0"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"0")),
                Frame::Array(vec![])
            ])
        );
    }

    #[test]
    fn hscan_with_a_non_numeric_cursor_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HSCAN", b"h", b"notacursor"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR invalid cursor".into())
        );
    }

    #[test]
    fn hscan_on_a_string_key_returns_wrongtype() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HSCAN", b"k", b"0"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error(
                "WRONGTYPE Operation against a key holding the wrong kind of value".into()
            )
        );
    }

    #[test]
    fn a_full_scan_over_dispatch_eventually_returns_cursor_zero() {
        let engine = Engine::new();
        for i in 0..50 {
            dispatch(
                &engine,
                cmd(&[b"SET", format!("k{i}").as_bytes(), b"v"]),
                &mut Protocol::default(),
                1,
            );
        }
        let mut cursor = Bytes::from_static(b"0");
        let mut total_keys = 0;
        loop {
            let reply = dispatch(
                &engine,
                cmd(&[b"SCAN", &cursor]),
                &mut Protocol::default(),
                1,
            );
            let Frame::Array(parts) = reply else {
                panic!("expected Array")
            };
            let Frame::Bulk(next) = parts[0].clone() else {
                panic!("expected Bulk cursor")
            };
            let Frame::Array(keys) = parts[1].clone() else {
                panic!("expected Array of keys")
            };
            total_keys += keys.len();
            cursor = next;
            if cursor.as_ref() == b"0" {
                break;
            }
        }
        assert_eq!(total_keys, 50);
    }

    #[test]
    fn zadd_then_zscore_round_trips_through_dispatch() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZADD", b"z", b"5", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZSCORE", b"z", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"5"))
        );
    }

    #[test]
    fn zadd_existing_member_returns_zero_and_updates_score() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"5", b"alice"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZADD", b"z", b"9", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(0)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZSCORE", b"z", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"9"))
        );
    }

    #[test]
    fn zadd_with_a_non_numeric_score_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZADD", b"z", b"notanumber", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR value is not a valid float".into())
        );
    }

    #[test]
    fn zadd_with_nan_or_infinite_score_is_a_resp_error() {
        let engine = Engine::new();
        for bad in [&b"nan"[..], &b"inf"[..], &b"-inf"[..]] {
            assert_eq!(
                dispatch(
                    &engine,
                    cmd(&[b"ZADD", b"z", bad, b"alice"]),
                    &mut Protocol::default(),
                    1
                ),
                Frame::Error("ERR value is not a valid float".into())
            );
        }
    }

    #[test]
    fn zscore_on_missing_member_returns_null() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZSCORE", b"z", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Null
        );
    }

    #[test]
    fn zrem_then_zcard_round_trip_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"5", b"alice"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"2", b"bob"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"ZCARD", b"z"]), &mut Protocol::default(), 1),
            Frame::Integer(2)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZREM", b"z", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"ZCARD", b"z"]), &mut Protocol::default(), 1),
            Frame::Integer(1)
        );
    }

    #[test]
    fn zincrby_returns_the_new_score_as_a_bulk_string() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"5", b"alice"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZINCRBY", b"z", b"3", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"8"))
        );
    }

    #[test]
    fn zscore_formats_a_fractional_score_without_trailing_zeros() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"5.5", b"alice"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZSCORE", b"z", b"alice"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"5.5"))
        );
    }

    #[test]
    fn zrange_returns_members_in_score_order_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"5", b"alice"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"2", b"bob"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZRANGE", b"z", b"0", b"-1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"bob")),
                Frame::Bulk(Bytes::from_static(b"alice")),
            ])
        );
    }

    #[test]
    fn zrange_with_a_non_integer_index_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZRANGE", b"z", b"notanumber", b"-1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR value is not an integer or out of range".into())
        );
    }

    #[test]
    fn zrank_returns_the_zero_based_position_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"5", b"alice"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"2", b"bob"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZRANK", b"z", b"bob"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(0)
        );
    }

    #[test]
    fn zrank_on_missing_member_returns_null() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"ZADD", b"z", b"5", b"alice"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"ZRANK", b"z", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Null
        );
    }

    #[test]
    fn lindex_lset_round_trip_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"a"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LINDEX", b"l", b"0"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"a"))
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LSET", b"l", b"0", b"z"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LINDEX", b"l", b"0"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"z"))
        );
    }

    #[test]
    fn lset_out_of_range_is_a_resp_error() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"a"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LSET", b"l", b"5", b"z"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR index out of range".into())
        );
    }

    #[test]
    fn ltrim_then_lrange_round_trip_through_dispatch() {
        let engine = Engine::new();
        for v in [b"a" as &[u8], b"b", b"c"] {
            dispatch(
                &engine,
                cmd(&[b"RPUSH", b"l", v]),
                &mut Protocol::default(),
                1,
            );
        }
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LTRIM", b"l", b"0", b"1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LRANGE", b"l", b"0", b"-1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"a")),
                Frame::Bulk(Bytes::from_static(b"b"))
            ])
        );
    }

    #[test]
    fn lrem_returns_the_count_removed_through_dispatch() {
        let engine = Engine::new();
        for v in [b"a" as &[u8], b"x", b"x"] {
            dispatch(
                &engine,
                cmd(&[b"RPUSH", b"l", v]),
                &mut Protocol::default(),
                1,
            );
        }
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LREM", b"l", b"0", b"x"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(2)
        );
    }

    #[test]
    fn linsert_before_and_after_work_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"a"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"c"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LINSERT", b"l", b"BEFORE", b"c", b"b"]),
                &mut Protocol::default(),
                1
            ),
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
                Frame::Bulk(Bytes::from_static(b"a")),
                Frame::Bulk(Bytes::from_static(b"b")),
                Frame::Bulk(Bytes::from_static(b"c")),
            ])
        );
    }

    #[test]
    fn linsert_with_an_invalid_direction_is_a_resp_error() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"a"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"LINSERT", b"l", b"SIDEWAYS", b"a", b"b"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR syntax error".into())
        );
    }

    #[test]
    fn hincrby_round_trips_through_dispatch() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HINCRBY", b"h", b"f", b"5"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(5)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HINCRBY", b"h", b"f", b"3"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(8)
        );
    }

    #[test]
    fn hincrby_on_a_non_integer_field_is_a_resp_error() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"HSET", b"h", b"f", b"abc"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HINCRBY", b"h", b"f", b"1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("value is not an integer or out of range".into())
        );
    }

    #[test]
    fn hkeys_and_hvals_round_trip_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"HSET", b"h", b"f", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"HKEYS", b"h"]), &mut Protocol::default(), 1),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"f"))])
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"HVALS", b"h"]), &mut Protocol::default(), 1),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"v"))])
        );
    }

    #[test]
    fn hmget_returns_null_for_missing_fields_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"HSET", b"h", b"f1", b"v1"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HMGET", b"h", b"f1", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"v1")), Frame::Null])
        );
    }

    #[test]
    fn hsetnx_returns_zero_when_the_field_already_exists() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"HSET", b"h", b"f", b"first"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HSETNX", b"h", b"f", b"second"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(0)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"HGET", b"h", b"f"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"first"))
        );
    }

    #[test]
    fn sinter_sunion_sdiff_round_trip_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SADD", b"a", b"x"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"SADD", b"a", b"y"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"SADD", b"b", b"y"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SINTER", b"a", b"b"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"y"))])
        );
        let Frame::Array(mut union) = dispatch(
            &engine,
            cmd(&[b"SUNION", b"a", b"b"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        union.sort_by_key(|f| format!("{f:?}"));
        assert_eq!(
            union,
            vec![
                Frame::Bulk(Bytes::from_static(b"x")),
                Frame::Bulk(Bytes::from_static(b"y"))
            ]
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SDIFF", b"a", b"b"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"x"))])
        );
    }

    #[test]
    fn sinterstore_stores_the_result_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SADD", b"a", b"x"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"SADD", b"b", b"x"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SINTERSTORE", b"dest", b"a", b"b"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SMEMBERS", b"dest"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"x"))])
        );
    }

    #[test]
    fn spop_removes_a_member_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SADD", b"s", b"x"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"SPOP", b"s"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"x"))
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"SCARD", b"s"]), &mut Protocol::default(), 1),
            Frame::Integer(0)
        );
    }

    #[test]
    fn spop_on_missing_key_returns_null() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SPOP", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Null
        );
    }

    #[test]
    fn srandmember_does_not_remove_the_member_through_dispatch() {
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
                cmd(&[b"SRANDMEMBER", b"s"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"x"))
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"SCARD", b"s"]), &mut Protocol::default(), 1),
            Frame::Integer(1)
        );
    }

    #[test]
    fn expire_sets_a_relative_ttl_and_ttl_reports_it_positive() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"EXPIRE", b"k", b"100"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        let Frame::Integer(secs) =
            dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1)
        else {
            panic!("expected Integer")
        };
        assert!((1..=100).contains(&secs));
    }

    #[test]
    fn expire_on_a_missing_key_returns_zero() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"EXPIRE", b"missing", b"100"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(0)
        );
    }

    #[test]
    fn expire_with_a_non_integer_seconds_is_a_resp_error() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"EXPIRE", b"k", b"soon"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR value is not an integer or out of range".into())
        );
    }

    #[test]
    fn pexpire_sets_a_millisecond_ttl() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"PEXPIRE", b"k", b"60000"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        let Frame::Integer(ms) =
            dispatch(&engine, cmd(&[b"PTTL", b"k"]), &mut Protocol::default(), 1)
        else {
            panic!("expected Integer")
        };
        assert!((1..=60000).contains(&ms));
    }

    #[test]
    fn expireat_with_a_past_timestamp_deletes_the_key_immediately() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"EXPIREAT", b"k", b"1"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
            Frame::Null
        );
    }

    #[test]
    fn pexpireat_with_a_future_timestamp_keeps_the_key_alive() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let future_ms = (std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .to_string();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"PEXPIREAT", b"k", future_ms.as_bytes()]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"GET", b"k"]), &mut Protocol::default(), 1),
            Frame::Bulk(Bytes::from_static(b"v"))
        );
    }

    #[test]
    fn ttl_on_a_missing_key_returns_negative_two() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"TTL", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(-2)
        );
    }

    #[test]
    fn ttl_on_a_key_with_no_expiry_returns_negative_one() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
            Frame::Integer(-1)
        );
    }

    #[test]
    fn persist_removes_an_existing_ttl_through_dispatch() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"EXPIRE", b"k", b"100"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"PERSIST", b"k"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
        assert_eq!(
            dispatch(&engine, cmd(&[b"TTL", b"k"]), &mut Protocol::default(), 1),
            Frame::Integer(-1)
        );
    }

    #[test]
    fn persist_on_a_key_with_no_ttl_returns_zero() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"PERSIST", b"k"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(0)
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

    use crate::aof::{AofWriter, FsyncPolicy};

    fn test_aof() -> (tempfile::TempDir, AofWriter) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.aof");
        let writer = AofWriter::open(&path, FsyncPolicy::Never).unwrap();
        (dir, writer)
    }

    fn read_aof(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("test.aof")).unwrap()
    }

    #[test]
    fn dispatch_and_log_appends_a_write_command_verbatim() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        aof.fsync().unwrap();
        assert_eq!(read_aof(&dir), "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
    }

    #[test]
    fn dispatch_and_log_does_not_log_a_read_only_command() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"GET", b"k"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        // only the one SET appears — GET never got appended
        assert_eq!(read_aof(&dir), "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
    }

    #[test]
    fn dispatch_and_log_does_not_log_a_write_command_that_errored() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        // wrong arg count -> Frame::Error, never reaches the engine
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SET", b"onlykey"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        assert_eq!(read_aof(&dir), "");
    }

    #[test]
    fn dispatch_and_log_rewrites_spop_to_srem_of_the_actually_popped_member() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SADD", b"s", b"x"]),
            &mut Protocol::default(),
            1,
        );
        let reply = dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SPOP", b"s"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"x"))); // the popped member
        aof.fsync().unwrap();
        let logged = read_aof(&dir);
        assert!(logged.ends_with("*3\r\n$4\r\nSREM\r\n$1\r\ns\r\n$1\r\nx\r\n"));
        assert!(!logged.contains("SPOP")); // the random command itself never hits the log
    }

    #[test]
    fn dispatch_and_log_does_not_log_spop_on_a_missing_key() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SPOP", b"missing"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        assert_eq!(read_aof(&dir), ""); // Frame::Null reply — nothing was popped, nothing to log
    }

    #[test]
    fn dispatch_and_log_rewrites_expire_to_an_absolute_pexpireat() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"EXPIRE", b"k", b"100"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        let logged = read_aof(&dir);
        assert!(logged.contains("PEXPIREAT"));
        assert!(!logged.contains("$6\r\nEXPIRE\r\n")); // the relative form never hits the log
    }

    #[test]
    fn dispatch_and_log_does_not_log_expire_on_a_missing_key() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"EXPIRE", b"missing", b"100"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        assert_eq!(read_aof(&dir), ""); // Frame::Integer(0) reply — nothing changed
    }

    #[test]
    fn dispatch_and_log_rewrites_set_with_ex_into_a_flagless_set_plus_pexpireat() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SET", b"k", b"v", b"EX", b"100"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        let logged = read_aof(&dir);
        // the SET is logged with EX/100 stripped, followed by an absolute PEXPIREAT
        assert!(logged.starts_with("*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"));
        assert!(logged.contains("PEXPIREAT"));
        assert!(!logged.contains("$2\r\nEX\r\n")); // the relative form never hits the log
    }

    #[test]
    fn dispatch_and_log_prefers_ex_over_px_when_both_are_present() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        // SET k v EX 100 PX 5000 — dispatch applies EX (100 seconds), not PX (5 seconds).
        // rewrite must also use EX, computing PEXPIREAT from 100 seconds, not 5000 milliseconds.
        let now_before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        dispatch_and_log(
            &engine,
            &aof,
            cmd(&[b"SET", b"k", b"v", b"EX", b"100", b"PX", b"5000"]),
            &mut Protocol::default(),
            1,
        );
        let now_after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        aof.fsync().unwrap();
        let logged = read_aof(&dir);
        // Verify SET is logged without TTL flags
        assert!(logged.starts_with("*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"));
        assert!(!logged.contains("$2\r\nEX\r\n"));
        assert!(!logged.contains("$2\r\nPX\r\n"));
        // The PEXPIREAT should be based on EX (100 seconds = 100,000 ms), not PX (5000 ms).
        // Parse the PEXPIREAT timestamp from the AOF and verify the delta from now is close to
        // 100,000 ms (EX wins), not 5,000 ms (which would indicate the buggy PX logic).
        assert!(logged.contains("PEXPIREAT"));
        // Find the PEXPIREAT frame and extract the target timestamp.
        // RESP format: *3\r\n$9\r\nPEXPIREAT\r\n$1\r\nk\r\n$<len>\r\n<timestamp>\r\n
        if let Some(pexpireat_pos) = logged.find("PEXPIREAT") {
            // Skip past "PEXPIREAT", find the key "k", then find the timestamp value.
            if let Some(after_k) = logged[pexpireat_pos..].find("$1\r\nk\r\n") {
                let search_from = pexpireat_pos + after_k + 7; // skip "$1\r\nk\r\n"
                                                               // Next RESP bulk string: $<len>\r\n<value>\r\n
                if let Some(len_pos) = logged[search_from..].find('$') {
                    let len_start = search_from + len_pos + 1;
                    if let Some(len_end) = logged[len_start..].find('\r') {
                        let len_str = &logged[len_start..len_start + len_end];
                        if let Ok(timestamp_len) = len_str.parse::<usize>() {
                            let timestamp_start = len_start + len_end + 2; // skip "\r\n"
                            let timestamp_end = timestamp_start + timestamp_len;
                            if timestamp_end <= logged.len() {
                                let timestamp_str = &logged[timestamp_start..timestamp_end];
                                if let Ok(target_unix_ms) = timestamp_str.parse::<i64>() {
                                    // Delta from now should be close to 100_000ms (100 seconds).
                                    let delta = target_unix_ms - now_before;
                                    // Allow 5-second tolerance for test execution time.
                                    let expected_delta = 100_000i64;
                                    let tolerance = 5_000i64;
                                    assert!(
                                        (delta - expected_delta).abs() < tolerance,
                                        "PEXPIREAT delta {} not close to EX=100s ({} ms); \
                                         indicates buggy PX precedence (would be ~5000 ms)",
                                        delta,
                                        expected_delta
                                    );
                                    return; // Test passed
                                }
                            }
                        }
                    }
                }
            }
        }
        panic!("Could not parse PEXPIREAT timestamp from AOF");
    }
}
