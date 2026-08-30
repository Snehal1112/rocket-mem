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
            for pair in pairs.as_chunks::<2>().0 {
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
            match commands::list::rpush(engine, rest[0].clone(), rest[1..].to_vec()) {
                Ok(n) => Frame::Integer(n as i64),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "LPUSH" => {
            require_args!(rest, 2, "lpush");
            match commands::list::lpush(engine, rest[0].clone(), rest[1..].to_vec()) {
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
            match commands::set::sadd(engine, rest[0].clone(), rest[1..].to_vec()) {
                Ok(n) => Frame::Integer(n),
                Err(e) => engine_error_to_frame(e),
            }
        }
        "SREM" => {
            require_args!(rest, 2, "srem");
            match commands::set::srem(engine, &rest[0], &rest[1..]) {
                Ok(n) => Frame::Integer(n),
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
            match engine.expire_at(&rest[0], common::instant_from_unix_ms(target_unix_ms)) {
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
        "MEMORY" => {
            require_args!(rest, 1, "memory");
            let subcommand = String::from_utf8_lossy(&rest[0]).to_ascii_uppercase();
            match subcommand.as_str() {
                "USAGE" => {
                    require_args!(rest, 2, "memory usage");
                    // with_ref, not get: sizing a value must not first clone the whole thing out
                    match engine.with_ref(&rest[1], |v| v.map(|v| v.approx_size())) {
                        Some(n) => Frame::Integer(n as i64),
                        None => Frame::Null,
                    }
                }
                _ => Frame::Error(format!("ERR unknown MEMORY subcommand '{subcommand}'")),
            }
        }
        "OBJECT" => {
            require_args!(rest, 1, "object");
            let subcommand = String::from_utf8_lossy(&rest[0]).to_ascii_uppercase();
            match subcommand.as_str() {
                "ENCODING" => {
                    require_args!(rest, 2, "object encoding");
                    // `type_name` returns &'static str, so nothing borrows past the closure
                    match engine.with_ref(&rest[1], |v| v.map(|v| v.type_name())) {
                        Some(name) => Frame::Bulk(Bytes::from(name)),
                        None => Frame::Error("ERR no such key".into()),
                    }
                }
                _ => Frame::Error(format!("ERR unknown OBJECT subcommand '{subcommand}'")),
            }
        }
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

/// Returns the uppercased command name from `frame` if it's one of `aof::WRITE_COMMANDS`,
/// else `None`. Computed before `dispatch` runs, so `dispatch_and_log` knows whether to hold
/// the AOF ordering lock without first having to inspect `reply`.
fn extract_write_command_name(frame: &Frame) -> Option<String> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return None;
    };
    let name = String::from_utf8_lossy(name_bytes).to_ascii_uppercase();
    crate::aof::WRITE_COMMANDS
        .contains(&name.as_str())
        .then_some(name)
}

/// Every command name this server answers -- `dispatch`'s match arms plus the interceptions
/// `dispatch_and_log` handles (`SAVE`, `REPLICAOF`, `PSYNC`, `CLUSTER`, `SLOWLOG`). Sorted, so
/// `binary_search` is valid; `known_commands_is_sorted_so_binary_search_works` is the guard that
/// keeps it that way when a future sprint adds one.
///
/// Two consumers: `key_spec` below (an unknown command has no keys, so it falls through to
/// dispatch's unknown-command error instead of being redirected on a slot computed from a
/// non-key argument), and `04-prometheus-metrics.md`'s `metric_label` (which collapses anything
/// not in this list to `other`, bounding Prometheus label cardinality).
///
/// **Every command added to `dispatch` from now on must be added here too.** A missing name is
/// not a compile error: it silently becomes `KeySpec::None`, which means that command is never
/// slot-routed in cluster mode -- it would be served by whichever node the client happened to
/// reach, quietly breaking the routing invariant. Step 3a below is the check.
#[allow(dead_code)] // dead_code until 02-cluster-commands-and-moved.md Task 3 wires in the real caller.
pub(crate) const KNOWN_COMMANDS: &[&str] = &[
    "APPEND",
    "CLUSTER",
    "COMMAND",
    "DECR",
    "DEL",
    "ECHO",
    "EXISTS",
    "EXPIRE",
    "EXPIREAT",
    "GET",
    "GETRANGE",
    "GETSET",
    "HDEL",
    "HELLO",
    "HEXISTS",
    "HGET",
    "HGETALL",
    "HINCRBY",
    "HKEYS",
    "HLEN",
    "HMGET",
    "HSCAN",
    "HSET",
    "HSETNX",
    "HVALS",
    "INCR",
    "INCRBY",
    "INFO",
    "KEYS",
    "LINDEX",
    "LINSERT",
    "LLEN",
    "LPOP",
    "LPUSH",
    "LRANGE",
    "LREM",
    "LSET",
    "LTRIM",
    "MEMORY",
    "MGET",
    "MSET",
    "MSETNX",
    "OBJECT",
    "PERSIST",
    "PEXPIRE",
    "PEXPIREAT",
    "PING",
    "PSYNC",
    "PTTL",
    "RANDOMKEY",
    "RENAME",
    "RENAMENX",
    "REPLICAOF",
    "RPOP",
    "RPUSH",
    "SADD",
    "SAVE",
    "SCAN",
    "SCARD",
    "SDIFF",
    "SDIFFSTORE",
    "SELECT",
    "SET",
    "SETRANGE",
    "SINTER",
    "SINTERSTORE",
    "SISMEMBER",
    "SLOWLOG",
    "SMEMBERS",
    "SPOP",
    "SRANDMEMBER",
    "SREM",
    "STRLEN",
    "SUNION",
    "SUNIONSTORE",
    "TTL",
    "TYPE",
    "ZADD",
    "ZCARD",
    "ZINCRBY",
    "ZRANGE",
    "ZRANK",
    "ZREM",
    "ZSCORE",
];

/// Which of a command's arguments are keys, for cluster-slot routing. Total over every command
/// this server answers; `First` is the default because it is correct for ~70 of the 84, and
/// every exception is enumerated in `key_spec`.
#[allow(dead_code)] // dead_code until 02-cluster-commands-and-moved.md Task 3 wires in the real caller.
enum KeySpec {
    /// No keys at all -- never redirected. Also the answer for unknown commands.
    None,
    /// The first argument (`GET k`, `SET k v`, `ZADD k ...`).
    First,
    /// The second argument (`MEMORY USAGE k`, `OBJECT ENCODING k`).
    Second,
    /// Every argument (`DEL a b c`, `RENAME a b`, `SINTERSTORE dest s1 s2` -- the destination is
    /// a key this node would write, so it must hash to the same slot as the sources).
    All,
    /// Arguments 0, 2, 4, ... (`MSET k1 v1 k2 v2`).
    EveryOther,
}

#[allow(dead_code)] // dead_code until 02-cluster-commands-and-moved.md Task 3 wires in the real caller.
fn key_spec(name: &str) -> KeySpec {
    match name {
        "PING" | "ECHO" | "SELECT" | "COMMAND" | "INFO" | "HELLO" | "KEYS" | "SCAN"
        | "RANDOMKEY" | "CLUSTER" | "SAVE" | "REPLICAOF" | "PSYNC" | "SLOWLOG" => KeySpec::None,
        "MEMORY" | "OBJECT" => KeySpec::Second,
        "DEL" | "EXISTS" | "MGET" | "RENAME" | "RENAMENX" | "SINTER" | "SUNION" | "SDIFF"
        | "SINTERSTORE" | "SUNIONSTORE" | "SDIFFSTORE" => KeySpec::All,
        "MSET" | "MSETNX" => KeySpec::EveryOther,
        _ if KNOWN_COMMANDS.binary_search(&name).is_ok() => KeySpec::First,
        _ => KeySpec::None, // unknown command: no keys, so dispatch's own error reaches the client
    }
}

/// The keys `frame`'s command operates on, borrowed from the frame. Empty for a malformed frame,
/// a keyless command, or an unknown command -- all three of which must reach their normal
/// handling rather than being redirected.
#[allow(dead_code)] // dead_code until 02-cluster-commands-and-moved.md Task 3 wires in the real caller.
fn command_keys(frame: &Frame) -> Vec<&Bytes> {
    let Frame::Array(items) = frame else {
        return Vec::new();
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return Vec::new();
    };
    let name = String::from_utf8_lossy(name_bytes).to_ascii_uppercase();
    let args: Vec<&Bytes> = items[1..]
        .iter()
        .filter_map(|f| match f {
            Frame::Bulk(b) => Some(b),
            _ => None,
        })
        .collect();
    match key_spec(&name) {
        KeySpec::None => Vec::new(),
        KeySpec::First => args.into_iter().take(1).collect(),
        KeySpec::Second => args.into_iter().skip(1).take(1).collect(),
        KeySpec::All => args,
        KeySpec::EveryOther => args.into_iter().step_by(2).collect(),
    }
}

fn is_save_command(frame: &Frame) -> bool {
    let Frame::Array(items) = frame else {
        return false;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return false;
    };
    name.eq_ignore_ascii_case(b"SAVE")
}

/// Returns `Some(reply)` if `frame` was `REPLICAOF` (in either form) — handled entirely here,
/// never reaching `dispatch` — or `None` if `frame` was some other command, in which case the
/// caller falls through to its normal handling.
fn handle_replicaof(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"REPLICAOF") {
        return None;
    }
    if items.len() != 3 {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'replicaof' command".into(),
        ));
    }
    let (Frame::Bulk(a), Frame::Bulk(b)) = (&items[1], &items[2]) else {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'replicaof' command".into(),
        ));
    };

    if a.eq_ignore_ascii_case(b"NO") && b.eq_ignore_ascii_case(b"ONE") {
        replication.stop_replicating();
    } else {
        let host = String::from_utf8_lossy(a);
        let port = String::from_utf8_lossy(b);
        replication.start_replicating(format!("{host}:{port}"));
    }
    Some(Frame::Simple("OK".into()))
}

/// Snapshots `replication.engine()` — in production this is always the same `Arc<Engine>` as
/// `dispatch_and_log`'s own `engine` parameter (`main.rs` constructs one `Engine`, shares it
/// into both `serve`'s `engine` argument and `ReplicationHandle::new`), so using the handle's
/// copy here matches the pattern `04-replica-registry-and-leader-fanout.md`'s `PSYNC` handling
/// already uses (`replication.engine().snapshot(0)`) instead of introducing a second,
/// redundant `&Engine` parameter that would always alias it anyway — and writes the result to
/// `replication.snapshot_path()`.
///
/// Holds `aof.lock_for_ordering()` across the offset read and the snapshot walk/encode (never
/// across the disk write) — see the sprint-5 spec's SAVE atomicity decision for why: without
/// this, a write landing between `current_offset()` and the snapshot walk would be captured in
/// both the snapshot and the AOF tail after the recorded offset, double-applying on a future
/// hybrid recovery for any non-idempotent command like `RPUSH`.
fn handle_save(
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Frame {
    let bytes = {
        let _order_guard = aof.lock_for_ordering();
        let offset = match aof.current_offset() {
            Ok(o) => o,
            Err(e) => return Frame::Error(format!("ERR failed to read AOF offset: {e}")),
        };
        replication.engine().snapshot(offset)
    };

    match write_snapshot_atomically(replication.snapshot_path(), &bytes) {
        Ok(()) => Frame::Simple("OK".into()),
        Err(e) => Frame::Error(format!("ERR failed to write snapshot: {e}")),
    }
}

/// Writes `bytes` to `<path>.tmp`, `sync_data`s it, then atomically renames it over `path`.
/// Without this, a crash partway through a direct write to `path` leaves a truncated file at
/// exactly the location startup will try to load next boot — `aof::recover` treats an
/// unreadable snapshot as a safe fallback to full AOF replay, so this never corrupts recovery,
/// but silently losing every snapshot on a crash defeats the feature's point.
fn write_snapshot_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut tmp_os = path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp_path = std::path::PathBuf::from(tmp_os);
    {
        let file = std::fs::File::create(&tmp_path)?;
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(bytes)?;
        writer.flush()?;
        writer.get_ref().sync_data()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Wraps `dispatch`, additionally appending successful write commands to `aof`. `dispatch`
/// itself is never modified — see ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md
/// for why AOF logging lives here instead of inside dispatch's own match arms.
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    // Checked before anything else in this function, including the SAVE/REPLICAOF
    // interceptions below (both are no-ops against WRITE_COMMANDS so ordering relative to
    // them doesn't matter) and extract_write_command_name's own later call further down (so
    // a rejected write never touches the AOF ordering lock).
    if replication
        .is_replica
        .load(std::sync::atomic::Ordering::Relaxed)
        && extract_write_command_name(&frame).is_some()
    {
        return Frame::Error("READONLY You can't write against a read only replica.".into());
    }

    if is_save_command(&frame) {
        return handle_save(aof, replication);
    }
    if let Some(reply) = handle_replicaof(&frame, replication) {
        return reply;
    }

    let original_frame = frame.clone();
    let write_name = extract_write_command_name(&original_frame);

    // Held across "mutate the engine, then log it" for write commands only, so two
    // concurrent connections' AOF appends always land in the order their mutations
    // committed in. Reads take no lock and stay fully concurrent. See
    // ../../docs/superpowers/specs/2026-08-30-tech-debt-cleanup-spec.md Item 2.
    let _order_guard = write_name.as_ref().map(|_| aof.lock_for_ordering());

    let reply = dispatch(engine, frame, protocol, client_id);
    if let Frame::Error(_) = reply {
        return reply;
    }
    let Some(name) = write_name else {
        return reply;
    };
    let Frame::Array(items) = &original_frame else {
        return reply;
    };

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
    // PEXPIREAT — on replay the key would live forever, silently dropping the TTL. Replay's
    // corrupt-tail handling (aof::replay) recovers a torn final frame, but it cannot detect
    // this case: both frames are individually well-formed, so the pair is not atomic.
    // Every frame still gets attempted even after a failure, so whatever can land on disk
    // does -- see the multi-frame note above about SET EX/PX's [SET, PEXPIREAT] pair.
    let mut aof_failed = false;
    for frame_to_log in to_log {
        // A logging failure must not fail the client's reply outright, but it must not be
        // silently swallowed either -- surface it so an operator watching stderr/logs can
        // notice a full disk or I/O error instead of discovering it only on replay. Under
        // Always, append() returns the writer thread's real I/O result, so a failed write or
        // fsync lands here and is also surfaced to the client below; under EverySecond/Never
        // the append is fire-and-forget, so this only catches a dead writer thread and the
        // writer itself reports I/O errors on stderr.
        let encoded = match crate::aof::encode_frame(&frame_to_log) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("aof encode failed: {e}");
                aof_failed = true;
                continue; // nothing to append or broadcast without a successful encode
            }
        };
        // One clone: `append_encoded` needs an owned `Vec<u8>` for the writer-thread channel
        // (AofMsg's existing shape, unchanged this sprint), while `broadcast` needs its own
        // `Bytes` handle. A small, accepted per-write-command cost rather than widening AofMsg's
        // channel type just for this sprint.
        if let Err(e) = aof.append_encoded(encoded.clone()) {
            eprintln!("aof append failed: {e}");
            aof_failed = true;
        }
        // Broadcast regardless of the append's result: the engine mutation already committed, so
        // a leader that fails to log a write locally must not also withhold it from its
        // replicas -- that would diverge them permanently over a purely local disk problem.
        replication.registry.broadcast(Bytes::from(encoded));
        // fsync timing for Always lives inside AofWriter::append itself; EverySecond's
        // periodic fsync loop lives in connection.rs (periodic_fsync_loop); Never does
        // nothing here.
    }
    // Only Always promises the client's reply won't precede durability -- EverySecond/Never
    // are fire-and-forget by design, so a write that hasn't landed yet is expected, not an
    // error to report back.
    if aof_failed && aof.policy() == crate::aof::FsyncPolicy::Always {
        return Frame::Error("ERR failed to write to the append only file".into());
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
        "EXPIRE" => now_ms.saturating_add(n.saturating_mul(1000)),
        "PEXPIRE" => now_ms.saturating_add(n),
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
            Frame::Bulk(Bytes::from(now_ms.saturating_add(ttl_ms).to_string())),
        ]),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::ReplicationHandle;
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
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        aof.fsync().unwrap();
        assert_eq!(read_aof(&dir), "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
    }

    #[test]
    fn dispatch_and_log_fans_out_a_write_command_to_registered_replicas() {
        let engine = std::sync::Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            dir.path().join("unused.snapshot"),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(tx);

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );

        let received = rx.try_recv().unwrap();
        assert_eq!(
            received.as_ref(),
            b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"
        );
    }

    #[test]
    fn dispatch_and_log_fans_out_spops_rewrite_not_the_original_command() {
        let engine = std::sync::Arc::new(Engine::new());
        dispatch(
            &engine,
            cmd(&[b"SADD", b"s", b"only-member"]),
            &mut Protocol::default(),
            1,
        );
        let (dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            dir.path().join("unused.snapshot"),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(tx);

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SPOP", b"s"]),
            &mut Protocol::default(),
            1,
        );

        let received = rx.try_recv().unwrap();
        assert_eq!(
            received.as_ref(),
            b"*3\r\n$4\r\nSREM\r\n$1\r\ns\r\n$11\r\nonly-member\r\n"
        );
    }

    #[test]
    fn dispatch_and_log_with_no_registered_replicas_still_succeeds() {
        let engine = std::sync::Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            dir.path().join("unused.snapshot"),
        );
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    #[test]
    fn dispatch_and_log_does_not_broadcast_a_read_only_command() {
        // a read-only command has no to_log entries at all -- broadcast must simply not be
        // reached for it, not error
        let engine = std::sync::Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            dir.path().join("unused.snapshot"),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(tx);

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"GET", b"k"]),
            &mut Protocol::default(),
            1,
        );

        assert!(rx.try_recv().is_err()); // nothing was broadcast for a read
    }

    #[test]
    fn dispatch_and_log_does_not_log_a_read_only_command() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
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
            &ReplicationHandle::default(),
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
            &ReplicationHandle::default(),
            cmd(&[b"SADD", b"s", b"x"]),
            &mut Protocol::default(),
            1,
        );
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
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
            &ReplicationHandle::default(),
            cmd(&[b"SPOP", b"missing"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        assert_eq!(read_aof(&dir), ""); // Frame::Null reply — nothing was popped, nothing to log
    }

    // `/dev/full` accepts an open and then fails every actual write with ENOSPC -- the same
    // deterministic stand-in for a full disk used in aof.rs's own propagation tests. Linux-only.
    #[cfg(target_os = "linux")]
    #[test]
    fn dispatch_and_log_returns_an_error_when_an_always_policy_aof_write_fails() {
        let engine = Engine::new();
        let aof = AofWriter::open(std::path::Path::new("/dev/full"), FsyncPolicy::Always)
            .expect("/dev/full opens fine; only writing to it fails");
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error("ERR failed to write to the append only file".into())
        );
        // The mutation still committed in memory -- it's durability that failed, not the write.
        assert_eq!(
            engine.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    // The single most important correctness property this task exists to guarantee: a leader
    // that fails to log a write locally must still fan it out to replicas -- withholding it
    // would silently diverge them over a purely local disk problem. `/dev/full` forces the AOF
    // append to fail while the engine mutation still commits, and a replica must still see the
    // encoded frame despite that failure.
    #[cfg(target_os = "linux")]
    #[test]
    fn dispatch_and_log_still_broadcasts_to_replicas_when_the_aof_append_fails() {
        let engine = std::sync::Arc::new(Engine::new());
        let aof = AofWriter::open(std::path::Path::new("/dev/full"), FsyncPolicy::Always)
            .expect("/dev/full opens fine; only writing to it fails");
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            std::path::PathBuf::from("unused.snapshot"),
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(tx);

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );

        // The reply reflects the AOF failure...
        assert_eq!(
            reply,
            Frame::Error("ERR failed to write to the append only file".into())
        );
        // ...but the replica still received the encoded frame -- broadcast is unconditional on
        // the append's result.
        let received = rx.try_recv().unwrap();
        assert_eq!(
            received.as_ref(),
            b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dispatch_and_log_still_returns_the_normal_reply_under_never_policy_even_when_the_disk_is_full(
    ) {
        // EverySecond/Never are fire-and-forget by design and never promised synchronous
        // durability, so a doomed write must not change the client-visible reply for them.
        let engine = Engine::new();
        let aof = AofWriter::open(std::path::Path::new("/dev/full"), FsyncPolicy::Never)
            .expect("/dev/full opens fine; only writing to it fails");
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    /// Parses the absolute-ms target out of a logged `PEXPIREAT <key> <ms>` frame in raw RESP
    /// wire bytes. Shared by tests that need to assert on the *computed* timestamp rather than
    /// merely that a PEXPIREAT is present — a rewrite that computes a wrong-but-present
    /// PEXPIREAT would otherwise still pass.
    fn parse_logged_pexpireat_ms(logged: &str, key: &str) -> i64 {
        let pexpireat_pos = logged.find("PEXPIREAT").expect("no PEXPIREAT in log");
        let key_marker = format!("${}\r\n{}\r\n", key.len(), key);
        let after_key = logged[pexpireat_pos..]
            .find(&key_marker)
            .expect("no matching key after PEXPIREAT");
        let search_from = pexpireat_pos + after_key + key_marker.len();
        // Next RESP bulk string: $<len>\r\n<value>\r\n
        let len_pos = logged[search_from..]
            .find('$')
            .expect("no length marker after key");
        let len_start = search_from + len_pos + 1;
        let len_end = logged[len_start..]
            .find('\r')
            .expect("no CR after bulk length");
        let timestamp_len: usize = logged[len_start..len_start + len_end]
            .parse()
            .expect("bulk length is not a number");
        let timestamp_start = len_start + len_end + 2; // skip "\r\n"
        let timestamp_end = timestamp_start + timestamp_len;
        logged[timestamp_start..timestamp_end]
            .parse()
            .expect("PEXPIREAT value is not an integer")
    }

    #[test]
    fn dispatch_and_log_rewrites_expire_to_an_absolute_pexpireat() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let now_before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"EXPIRE", b"k", b"100"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        let logged = read_aof(&dir);
        assert!(logged.contains("PEXPIREAT"));
        assert!(!logged.contains("$6\r\nEXPIRE\r\n")); // the relative form never hits the log

        // EXPIRE k 100 -> PEXPIREAT delta from "now" should be close to 100_000ms.
        let target_ms = parse_logged_pexpireat_ms(&logged, "k");
        let delta = target_ms - now_before;
        assert!(
            (delta - 100_000).abs() < 5_000,
            "PEXPIREAT delta {delta} not close to the expected 100_000ms"
        );
    }

    #[test]
    fn dispatch_and_log_rewrites_expireat_to_an_exact_absolute_pexpireat() {
        // EXPIREAT/PEXPIREAT take a structurally different, purely-absolute branch (a plain
        // seconds->ms conversion with no `now_ms` term at all), so unlike EXPIRE/PEXPIRE this
        // can be asserted as an exact value rather than a delta-from-now with tolerance.
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"EXPIREAT", b"k", b"2000000000"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        let logged = read_aof(&dir);
        assert!(logged.contains("PEXPIREAT"));
        let target_ms = parse_logged_pexpireat_ms(&logged, "k");
        assert_eq!(target_ms, 2_000_000_000_000);
    }

    #[test]
    fn dispatch_and_log_does_not_log_expire_on_a_missing_key() {
        let engine = Engine::new();
        let (dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
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
            &ReplicationHandle::default(),
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
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v", b"EX", b"100", b"PX", b"5000"]),
            &mut Protocol::default(),
            1,
        );
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

    #[test]
    fn dispatch_and_log_does_not_panic_on_an_expire_ttl_that_would_overflow_i64() {
        // Regression test: `now_ms + n.saturating_mul(1000)` used to be a plain, panicking
        // (debug) / wrapping (release) addition in rewrite_expire_family_to_pexpireat. A huge
        // but syntactically valid TTL like this one drives now_ms + (n * 1000) past i64::MAX,
        // which previously panicked live against the built server:
        //   SET k v -> +OK
        //   EXPIRE k 10000000000000000 -> connection dropped, no reply
        //   thread panicked at dispatcher.rs:1022:21: attempt to add with overflow
        // Completing without panicking proves the fix; the resulting (saturated) timestamp
        // isn't a meaningful real-world value, so it's not asserted on.
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"EXPIRE", b"k", b"10000000000000000"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Integer(1)); // key existed, so the TTL was applied
        aof.fsync().unwrap();
    }

    #[test]
    fn memory_usage_reports_the_approximate_size_of_an_existing_key() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let Frame::Integer(n) = dispatch(
            &engine,
            cmd(&[b"MEMORY", b"USAGE", b"k"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Integer")
        };
        assert!(n > 0);
    }

    #[test]
    fn memory_usage_on_a_missing_key_returns_null() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"MEMORY", b"USAGE", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Null
        );
    }

    #[test]
    fn memory_with_an_unknown_subcommand_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"MEMORY", b"NOPE"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown MEMORY subcommand 'NOPE'".into())
        );
    }

    #[test]
    fn object_encoding_reports_a_type_derived_name_for_each_value_type() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SET", b"s", b"v"]),
            &mut Protocol::default(),
            1,
        );
        dispatch(
            &engine,
            cmd(&[b"RPUSH", b"l", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"OBJECT", b"ENCODING", b"s"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"string"))
        );
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"OBJECT", b"ENCODING", b"l"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"list"))
        );
    }

    #[test]
    fn object_encoding_on_a_missing_key_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"OBJECT", b"ENCODING", b"missing"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR no such key".into())
        );
    }

    #[test]
    fn object_with_an_unknown_subcommand_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"OBJECT", b"NOPE", b"k"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown OBJECT subcommand 'NOPE'".into())
        );
    }

    #[test]
    fn save_writes_a_snapshot_that_load_snapshot_can_read_back() {
        let engine = std::sync::Arc::new(Engine::new());
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let (dir, aof) = test_aof();
        let snapshot_path = dir.path().join("test.snapshot");
        let replication =
            ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SAVE"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));

        let bytes = std::fs::read(&snapshot_path).unwrap();
        let loaded = Engine::new();
        loaded.load_snapshot(&bytes).unwrap();
        assert_eq!(
            loaded.get(b"k"),
            Some(Value::String(Bytes::from_static(b"v")))
        );
    }

    #[test]
    fn save_does_not_leave_a_tmp_file_behind_on_success() {
        let engine = std::sync::Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let snapshot_path = dir.path().join("test.snapshot");
        let replication =
            ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path.clone());

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SAVE"]),
            &mut Protocol::default(),
            1,
        );

        let mut tmp = snapshot_path.clone().into_os_string();
        tmp.push(".tmp");
        assert!(!std::path::Path::new(&tmp).exists());
    }

    #[test]
    fn save_is_not_appended_to_the_aof() {
        let engine = std::sync::Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let snapshot_path = dir.path().join("test.snapshot");
        let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path);

        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SAVE"]),
            &mut Protocol::default(),
            1,
        );
        aof.fsync().unwrap();
        assert_eq!(read_aof(&dir), ""); // SAVE has nothing to replay -- it must not appear in the AOF
    }

    #[test]
    fn save_holds_the_ordering_lock_across_the_offset_read_and_the_snapshot_walk() {
        // Proves the lock is load-bearing, not just present. Without it, a concurrent RPUSH
        // can land between SAVE's offset read and its snapshot walk, so that push is captured
        // in BOTH the snapshot AND the AOF tail after the recorded offset -- a hybrid recovery
        // then replays it a second time, corrupting the list with a duplicate element. RPUSH
        // is used (not e.g. SET) because replaying it twice is observably wrong, unlike an
        // idempotent command.
        const PUSHES: usize = 2000;

        let engine = std::sync::Arc::new(Engine::new());
        let dir = tempfile::tempdir().unwrap();
        let aof_path = dir.path().join("test.aof");
        let aof = std::sync::Arc::new(AofWriter::open(&aof_path, FsyncPolicy::Never).unwrap());
        let snapshot_path = dir.path().join("test.snapshot");
        let replication = std::sync::Arc::new(ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            snapshot_path.clone(),
        ));

        let pusher = {
            let engine = std::sync::Arc::clone(&engine);
            let aof = std::sync::Arc::clone(&aof);
            let replication = std::sync::Arc::clone(&replication);
            std::thread::spawn(move || {
                for _ in 0..PUSHES {
                    dispatch_and_log(
                        &engine,
                        &aof,
                        &replication,
                        cmd(&[b"RPUSH", b"list", b"x"]),
                        &mut Protocol::default(),
                        1,
                    );
                }
            })
        };

        // Issued concurrently with the pusher thread above, not after joining it.
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SAVE"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));

        pusher.join().unwrap();
        aof.fsync().unwrap();

        let recovered = crate::aof::recover(&aof_path, &snapshot_path).unwrap();
        let len = match recovered.get(b"list") {
            Some(Value::List(l)) => l.len(),
            other => panic!("expected a List with {PUSHES} elements, got {other:?}"),
        };
        assert_eq!(len, PUSHES); // a lost guard shows up here as a duplicated element
    }

    #[tokio::test]
    async fn replicaof_with_host_and_port_returns_ok_and_marks_the_node_a_replica() {
        let engine = std::sync::Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            "/tmp/unused.snapshot".into(),
        );

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"REPLICAOF", b"127.0.0.1", b"1"]), // port 1: nothing listens there, connection attempt fails harmlessly in the background
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        assert!(replication
            .is_replica
            .load(std::sync::atomic::Ordering::Relaxed));
        replication.stop_replicating(); // clean up the background task this test started
    }

    #[tokio::test]
    async fn replicaof_no_one_returns_ok_and_clears_replica_status() {
        let engine = std::sync::Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            "/tmp/unused.snapshot".into(),
        );
        replication.start_replicating("127.0.0.1:1".to_string());

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"REPLICAOF", b"NO", b"ONE"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        assert!(!replication
            .is_replica
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn replicaof_with_the_wrong_number_of_arguments_is_a_resp_error() {
        let engine = std::sync::Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            "/tmp/unused.snapshot".into(),
        );
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"REPLICAOF", b"onlyhost"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error("ERR wrong number of arguments for 'replicaof' command".into())
        );
    }

    #[test]
    fn a_write_command_on_a_replica_is_rejected_with_readonly() {
        let engine = std::sync::Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            "/tmp/unused.snapshot".into(),
        );
        replication
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error("READONLY You can't write against a read only replica.".into())
        );
        assert_eq!(engine.get(b"k"), None); // the write must never have reached the engine
    }

    #[test]
    fn a_read_command_on_a_replica_is_not_gated() {
        let engine = std::sync::Arc::new(Engine::new());
        dispatch(
            &engine,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            "/tmp/unused.snapshot".into(),
        );
        replication
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"GET", b"k"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Bulk(Bytes::from_static(b"v")));
    }

    #[test]
    fn save_is_not_gated_on_a_replica() {
        let engine = std::sync::Arc::new(Engine::new());
        let (dir, aof) = test_aof();
        let snapshot_path = dir.path().join("test.snapshot");
        let replication = ReplicationHandle::new(std::sync::Arc::clone(&engine), snapshot_path);
        replication
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SAVE"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    #[test]
    fn a_write_command_when_not_a_replica_is_unaffected() {
        let engine = std::sync::Arc::new(Engine::new());
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::new(
            std::sync::Arc::clone(&engine),
            "/tmp/unused.snapshot".into(),
        );

        let reply = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    #[test]
    fn known_commands_is_sorted_so_binary_search_works() {
        let mut sorted = KNOWN_COMMANDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, KNOWN_COMMANDS.to_vec());
        assert!(KNOWN_COMMANDS.binary_search(&"GET").is_ok());
        assert!(KNOWN_COMMANDS.binary_search(&"ZSCORE").is_ok());
        assert!(KNOWN_COMMANDS.binary_search(&"NOSUCHCOMMAND").is_err());
    }

    #[test]
    fn command_keys_finds_the_single_key_of_an_ordinary_command() {
        assert_eq!(
            command_keys(&cmd(&[b"GET", b"foo"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"SET", b"foo", b"bar", b"EX", b"10"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"HSET", b"h", b"field", b"value"])),
            vec![&Bytes::from_static(b"h")]
        );
    }

    #[test]
    fn command_keys_is_empty_for_commands_that_take_no_key() {
        for c in [
            cmd(&[b"PING"]),
            cmd(&[b"ECHO", b"hello"]),
            cmd(&[b"SELECT", b"0"]),
            cmd(&[b"COMMAND"]),
            cmd(&[b"INFO", b"replication"]),
            cmd(&[b"HELLO", b"3"]),
            cmd(&[b"KEYS", b"*"]),
            cmd(&[b"SCAN", b"0"]),
            cmd(&[b"RANDOMKEY"]),
            cmd(&[b"CLUSTER", b"KEYSLOT", b"foo"]),
            cmd(&[b"SAVE"]),
            cmd(&[b"REPLICAOF", b"NO", b"ONE"]),
            cmd(&[b"PSYNC"]),
            cmd(&[b"SLOWLOG", b"GET"]),
        ] {
            assert!(command_keys(&c).is_empty(), "expected no keys for {c:?}");
        }
    }

    #[test]
    fn command_keys_is_empty_for_an_unknown_command() {
        // An unknown command must fall through to dispatch's "ERR unknown command" error, not
        // get redirected on a slot computed from an argument that isn't a key.
        assert!(command_keys(&cmd(&[b"NOSUCHCOMMAND", b"foo"])).is_empty());
    }

    #[test]
    fn command_keys_takes_the_second_argument_for_memory_usage_and_object_encoding() {
        assert_eq!(
            command_keys(&cmd(&[b"MEMORY", b"USAGE", b"foo"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"OBJECT", b"ENCODING", b"foo"])),
            vec![&Bytes::from_static(b"foo")]
        );
        assert!(command_keys(&cmd(&[b"MEMORY"])).is_empty());
    }

    #[test]
    fn command_keys_takes_every_argument_for_variadic_key_commands() {
        assert_eq!(
            command_keys(&cmd(&[b"DEL", b"a", b"b", b"c"])),
            vec![
                &Bytes::from_static(b"a"),
                &Bytes::from_static(b"b"),
                &Bytes::from_static(b"c")
            ]
        );
        assert_eq!(
            command_keys(&cmd(&[b"MGET", b"a", b"b"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"RENAME", b"a", b"b"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
        // the destination is a key this node would WRITE, so it must be routed too
        assert_eq!(
            command_keys(&cmd(&[b"SINTERSTORE", b"dest", b"s1", b"s2"])),
            vec![
                &Bytes::from_static(b"dest"),
                &Bytes::from_static(b"s1"),
                &Bytes::from_static(b"s2")
            ]
        );
    }

    #[test]
    fn command_keys_takes_every_other_argument_for_mset() {
        assert_eq!(
            command_keys(&cmd(&[b"MSET", b"a", b"1", b"b", b"2"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
        assert_eq!(
            command_keys(&cmd(&[b"MSETNX", b"a", b"1", b"b", b"2"])),
            vec![&Bytes::from_static(b"a"), &Bytes::from_static(b"b")]
        );
    }
}
