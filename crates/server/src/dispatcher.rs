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

/// No command this server answers is longer than 12 bytes (`SUNIONSTORE`, `SRANDMEMBER`); 32 is
/// generous headroom that still fits comfortably on the stack.
pub(crate) const MAX_COMMAND_NAME_LEN: usize = 32;

/// A command name uppercased into a fixed stack buffer. Exists to remove the two-to-four heap
/// allocations every single command used to pay for its own name -- `dispatch`,
/// `extract_write_command_name`, the metrics wrapper, and the cluster routing gate each did
/// `String::from_utf8_lossy(..).to_ascii_uppercase()` independently. See
/// ../../docs/benchmarks/2026-08-30-flamegraph-notes.md for the profile that motivated it.
pub(crate) struct CommandName {
    buf: [u8; MAX_COMMAND_NAME_LEN],
    len: usize,
}

impl CommandName {
    pub(crate) fn as_str(&self) -> &str {
        // `upper_name` accepts only ASCII, so this cannot fail.
        std::str::from_utf8(&self.buf[..self.len]).expect("upper_name accepts only ASCII input")
    }
}

/// Uppercases `raw` into a `CommandName`, or `None` if it cannot be a command name at all --
/// longer than `MAX_COMMAND_NAME_LEN`, or non-ASCII. Both cases are necessarily unknown
/// commands, and callers handle them on their cold path.
pub(crate) fn upper_name(raw: &[u8]) -> Option<CommandName> {
    if raw.len() > MAX_COMMAND_NAME_LEN || !raw.is_ascii() {
        return None;
    }
    let mut buf = [0u8; MAX_COMMAND_NAME_LEN];
    for (slot, byte) in buf.iter_mut().zip(raw) {
        *slot = byte.to_ascii_uppercase();
    }
    Some(CommandName {
        buf,
        len: raw.len(),
    })
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

pub fn dispatch(engine: &Engine, frame: Frame, _protocol: &mut Protocol, _client_id: u64) -> Frame {
    let args = match frame_to_args(frame) {
        Ok(a) => a,
        Err(e) => return e,
    };
    if args.is_empty() {
        return Frame::Error("ERR empty command".into());
    }
    let Some(name) = upper_name(&args[0]) else {
        // Cold path only: a name too long or non-ASCII to be any command we know. The error text
        // is unchanged from before this optimization -- it echoes the client's own bytes.
        return Frame::Error(format!(
            "ERR unknown command '{}'",
            String::from_utf8_lossy(&args[0])
        ));
    };
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
        "SSCAN" => {
            require_args!(rest, 2, "sscan");
            // No MATCH/COUNT support yet, matching HSCAN's current scope. A set already lives
            // fully in memory (SMEMBERS reads it all in one shot), so like HSCAN there's no
            // chunking to design here -- one call always returns everything and reports cursor
            // "0" (done), which is a legitimate SCAN-family reply.
            if std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .is_none()
            {
                return Frame::Error("ERR invalid cursor".into());
            }
            match commands::set::smembers(engine, &rest[0]) {
                Ok(members) => Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"0")),
                    Frame::Array(members.into_iter().map(Frame::Bulk).collect()),
                ]),
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
            require_args!(rest, 2, name.as_str().to_ascii_lowercase());
            let n: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            let delta = if name.as_str() == "EXPIRE" {
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
            require_args!(rest, 2, name.as_str().to_ascii_lowercase());
            let n: i64 = match std::str::from_utf8(&rest[1])
                .ok()
                .and_then(|s| s.parse().ok())
            {
                Some(n) => n,
                None => return Frame::Error("ERR value is not an integer or out of range".into()),
            };
            let target_unix_ms = if name.as_str() == "EXPIREAT" {
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
        _ => Frame::Error(format!("ERR unknown command '{}'", name.as_str())),
    }
}

fn hello_reply(
    protocol: Protocol,
    client_id: u64,
    role: &'static str,
    mode: &'static str,
) -> Frame {
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
            Frame::Bulk(Bytes::from(mode)),
        ),
        (
            Frame::Bulk(Bytes::from_static(b"role")),
            Frame::Bulk(Bytes::from(role)),
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
fn extract_write_command_name(frame: &Frame) -> Option<CommandName> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return None;
    };
    let name = upper_name(name_bytes)?;
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
fn command_keys(frame: &Frame) -> Vec<&Bytes> {
    let Frame::Array(items) = frame else {
        return Vec::new();
    };
    let Some(Frame::Bulk(name_bytes)) = items.first() else {
        return Vec::new();
    };
    let Some(name) = upper_name(name_bytes) else {
        return Vec::new(); // not a command name we know, so it has no keys to route
    };
    let args: Vec<&Bytes> = items[1..]
        .iter()
        .filter_map(|f| match f {
            Frame::Bulk(b) => Some(b),
            _ => None,
        })
        .collect();
    match key_spec(name.as_str()) {
        KeySpec::None => Vec::new(),
        KeySpec::First => args.into_iter().take(1).collect(),
        KeySpec::Second => args.into_iter().skip(1).take(1).collect(),
        KeySpec::All => args,
        KeySpec::EveryOther => args.into_iter().step_by(2).collect(),
    }
}

/// `None` = this node may handle the command. `Some(frame)` = reply with this instead, without
/// touching the engine, the AOF, the replica fan-out, or any lock.
///
/// Called only from `dispatch_and_log`, never from `dispatch`: `aof::replay` and the follower
/// apply loop call `dispatch` directly and must apply every frame they are handed regardless of
/// slot ownership -- redirecting there would silently drop writes during recovery and
/// replication. Keeping the check here makes that impossible by construction.
///
/// When cluster mode is off (the default, and every existing test), this is one `Option` check.
fn cluster_redirect(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let cluster = replication.cluster()?;
    let keys = command_keys(frame);
    let mut slots = keys.into_iter().map(|k| crate::cluster::key_slot(k));
    let first = slots.next()?; // no keys => nothing to route
    if !slots.all(|s| s == first) {
        // Without this, `MSET a 1 b 2` across two slots would be accepted by whichever node owns
        // `a` and would then write `b` onto a node that does not own it -- a silent, permanent
        // violation of the routing invariant, undetectable by any client. Hash tags are how a
        // client legitimately keeps multi-key commands working under this rule.
        return Some(Frame::Error(
            "CROSSSLOT Keys in request don't hash to the same slot".into(),
        ));
    }
    if cluster.owns(first) {
        return None;
    }
    let owner = cluster.owner_of(first);
    Some(Frame::Error(format!("MOVED {first} {}", owner.addr)))
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

/// Splits a config `host:port` into its parts. Falls back to the whole string and port 0 on a
/// malformed address; `ClusterConfig::parse` does not validate the address shape (it is echoed
/// to clients verbatim, so it must not be normalized), and this is the one place that needs the
/// halves separately.
fn split_addr(addr: &str) -> (&str, i64) {
    match addr.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().unwrap_or(0)),
        None => (addr, 0),
    }
}

/// `CLUSTER INFO`'s body. `cluster_state` is unconditionally `ok` and every epoch is
/// unconditionally `0` because a static config has no way to know otherwise -- there is no
/// gossip to learn a peer is down, and no epoch bumping without resharding or failover. Pinning
/// the fields we cannot compute to the value that is true by construction beats fabricating one.
fn cluster_info_text(cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>) -> String {
    let (enabled, assigned, count) = match cluster {
        Some(c) => (1, crate::cluster::SLOT_COUNT as u32, c.nodes().len()),
        None => (0, 0, 0),
    };
    format!(
        "cluster_enabled:{enabled}\r\n\
         cluster_state:ok\r\n\
         cluster_slots_assigned:{assigned}\r\n\
         cluster_known_nodes:{count}\r\n\
         cluster_size:{count}\r\n\
         cluster_my_epoch:0\r\n\
         cluster_current_epoch:0\r\n"
    )
}

/// `CLUSTER NODES`'s body, one `\n`-terminated line per node in real Redis's space-separated
/// format (that payload uses `\n`, not `\r\n`, inside the bulk string). The `@<cport>` cluster-bus
/// port is the Redis convention of `port + 10000`; it is **advertised but never bound**, because
/// there is no cluster bus -- the field is not optional in the grammar clients parse, so the
/// conventional value is emitted and the caveat is recorded in the README. `connected` is
/// likewise unconditional: nothing here can observe a peer disconnecting.
fn cluster_nodes_text(cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>) -> String {
    let Some(cluster) = cluster else {
        return String::new();
    };
    let my_id = &cluster.myself().id;
    cluster
        .nodes()
        .iter()
        .map(|n| {
            let (_, port) = split_addr(&n.addr);
            let flags = if &n.id == my_id {
                "myself,master"
            } else {
                "master"
            };
            format!(
                "{} {}@{} {} - 0 0 0 connected {}-{}\n",
                n.id,
                n.addr,
                port + 10000,
                flags,
                n.first_slot,
                n.last_slot
            )
        })
        .collect()
}

/// `CLUSTER SHARDS`'s reply: one entry per configured node, each an `Array` of alternating
/// key/value frames rather than a `Map`, so RESP2 and RESP3 clients see identical output and
/// this helper needs no `Protocol` state. `role` is always `master` and each shard has exactly
/// one node: this sprint's cluster has no shard-level replicas. `replication-offset` is 0
/// because this project has no replication offsets at all (Sprint 5 made every resync a full
/// one), and the field is present only because clients parse for it.
fn cluster_shards_reply(cluster: Option<&std::sync::Arc<crate::cluster::ClusterConfig>>) -> Frame {
    let Some(cluster) = cluster else {
        return Frame::Array(vec![]);
    };
    Frame::Array(
        cluster
            .nodes()
            .iter()
            .map(|n| {
                let (host, port) = split_addr(&n.addr);
                let node = Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"id")),
                    Frame::Bulk(Bytes::from(n.id.clone())),
                    Frame::Bulk(Bytes::from_static(b"port")),
                    Frame::Integer(port),
                    Frame::Bulk(Bytes::from_static(b"ip")),
                    Frame::Bulk(Bytes::from(host.to_string())),
                    Frame::Bulk(Bytes::from_static(b"endpoint")),
                    Frame::Bulk(Bytes::from(host.to_string())),
                    Frame::Bulk(Bytes::from_static(b"role")),
                    Frame::Bulk(Bytes::from_static(b"master")),
                    Frame::Bulk(Bytes::from_static(b"replication-offset")),
                    Frame::Integer(0),
                    Frame::Bulk(Bytes::from_static(b"health")),
                    Frame::Bulk(Bytes::from_static(b"online")),
                ]);
                Frame::Array(vec![
                    Frame::Bulk(Bytes::from_static(b"slots")),
                    Frame::Array(vec![
                        Frame::Integer(n.first_slot as i64),
                        Frame::Integer(n.last_slot as i64),
                    ]),
                    Frame::Bulk(Bytes::from_static(b"nodes")),
                    Frame::Array(vec![node]),
                ])
            })
            .collect(),
    )
}

/// Returns `Some(reply)` if `frame` was a `CLUSTER` command -- handled entirely here, never
/// reaching `dispatch` -- or `None` if it was some other command. Same interception shape as
/// `handle_replicaof` above, and for the same reason: this needs `ReplicationHandle`, which
/// plain `dispatch` has no parameter for.
///
/// `CLUSTER KEYSLOT` is answered even when cluster mode is off: it is a pure function of the
/// key, real Redis answers it in non-cluster mode too, and making it conditional would leave
/// this sprint's headline algorithm untestable over the wire on a plain node.
fn handle_cluster(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"CLUSTER") {
        return None;
    }
    let Some(Frame::Bulk(sub_bytes)) = items.get(1) else {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'cluster' command".into(),
        ));
    };
    let sub = String::from_utf8_lossy(sub_bytes).to_ascii_uppercase();
    let cluster = replication.cluster();
    Some(match sub.as_str() {
        "KEYSLOT" => match items.get(2) {
            Some(Frame::Bulk(key)) if items.len() == 3 => {
                Frame::Integer(crate::cluster::key_slot(key) as i64)
            }
            _ => Frame::Error("ERR wrong number of arguments for 'cluster|keyslot' command".into()),
        },
        "MYID" => Frame::Bulk(match cluster {
            Some(c) => Bytes::from(c.myself().id.clone()),
            // 40 zeroes: real Redis's "no cluster identity" shape, rather than inventing one.
            None => Bytes::from("0".repeat(40)),
        }),
        "INFO" => Frame::Bulk(Bytes::from(cluster_info_text(cluster))),
        "SHARDS" => cluster_shards_reply(cluster),
        "NODES" => Frame::Bulk(Bytes::from(cluster_nodes_text(cluster))),
        _ => Frame::Error(format!("ERR unknown CLUSTER subcommand '{sub}'")),
    })
}

/// Real Redis's human-readable byte format, e.g. `80.00K`. Purely cosmetic -- `used_memory` is
/// the machine-readable field; tooling that graphs memory reads that one.
fn human_bytes(bytes: usize) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K {
        format!("{:.2}G", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.2}M", b / (K * K))
    } else if b >= K {
        format!("{:.2}K", b / K)
    } else {
        format!("{bytes}B")
    }
}

/// The `INFO persistence` name for an fsync policy, matching real Redis's spelling
/// (`always`/`everysec`/`no`) rather than this codebase's enum names.
fn fsync_policy_name(policy: crate::aof::FsyncPolicy) -> &'static str {
    match policy {
        crate::aof::FsyncPolicy::Always => "always",
        crate::aof::FsyncPolicy::EverySecond => "everysec",
        crate::aof::FsyncPolicy::Never => "no",
    }
}

/// Builds `INFO`'s body. `section` is `None` for "every section" (no argument, or `all`/
/// `default`/`everything`), otherwise a lowercase section name.
///
/// Every field here is backed by state this server actually tracks. Fields real Redis has that
/// this one cannot compute -- `keyspace_hits`/`keyspace_misses` (nothing counts them),
/// `tcp_port` (the dispatcher never learns the listen address), `rdb_changes_since_last_save`
/// -- are omitted rather than faked.
fn info_text(
    section: Option<&str>,
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> String {
    let wanted = |name: &str| section.is_none() || section == Some(name);
    let mut out = String::new();

    if wanted("server") {
        let uptime = replication.uptime_secs();
        out.push_str(&format!(
            "# Server\r\n\
             redis_version:rocket-mem-{version}\r\n\
             rocket_mem_version:{version}\r\n\
             redis_mode:{mode}\r\n\
             os:{os}\r\n\
             arch_bits:{bits}\r\n\
             process_id:{pid}\r\n\
             uptime_in_seconds:{uptime}\r\n\
             uptime_in_days:{days}\r\n\r\n",
            version = env!("CARGO_PKG_VERSION"),
            mode = if replication.cluster().is_some() {
                "cluster"
            } else {
                "standalone"
            },
            os = std::env::consts::OS,
            bits = usize::BITS,
            pid = std::process::id(),
            days = uptime / 86_400,
        ));
    }

    if wanted("clients") {
        out.push_str(&format!(
            "# Clients\r\nconnected_clients:{}\r\n\r\n",
            replication.connected_clients()
        ));
    }

    if wanted("memory") {
        let used = engine.memory_used();
        out.push_str(&format!(
            "# Memory\r\n\
             used_memory:{used}\r\n\
             used_memory_human:{}\r\n\
             maxmemory:{}\r\n\
             maxmemory_policy:allkeys-lru\r\n\r\n",
            human_bytes(used),
            engine.maxmemory().unwrap_or(0),
        ));
    }

    if wanted("persistence") {
        out.push_str(&format!(
            "# Persistence\r\n\
             aof_enabled:1\r\n\
             aof_fsync_policy:{}\r\n\
             rdb_last_save_time:{}\r\n\
             rdb_bgsave_in_progress:0\r\n\r\n",
            fsync_policy_name(aof.policy()),
            replication.last_save_unix(),
        ));
    }

    if wanted("stats") {
        out.push_str(&format!(
            "# Stats\r\n\
             total_connections_received:{}\r\n\
             total_commands_processed:{}\r\n\
             expired_keys:{}\r\n\
             evicted_keys:{}\r\n\r\n",
            replication.total_connections(),
            replication.total_commands(),
            replication.expired_keys(),
            engine.eviction_count(),
        ));
    }

    if wanted("replication") {
        let is_replica = replication
            .is_replica
            .load(std::sync::atomic::Ordering::Relaxed);
        out.push_str("# Replication\r\n");
        if is_replica {
            // `slave`, not `replica`: real Redis still emits the legacy word and every client
            // library parses for it. Matching the wire is the point.
            out.push_str("role:slave\r\n");
            if let Some(addr) = replication.master_addr() {
                let (host, port) = split_addr(&addr);
                out.push_str(&format!("master_host:{host}\r\nmaster_port:{port}\r\n"));
            }
            out.push_str(&format!(
                "master_link_status:{}\r\n",
                if replication.link_up() { "up" } else { "down" }
            ));
        } else {
            out.push_str("role:master\r\n");
            out.push_str(&format!(
                "connected_slaves:{}\r\n",
                replication.registry.len()
            ));
        }
        out.push_str("\r\n");
    }

    if wanted("cluster") {
        out.push_str(&format!(
            "# Cluster\r\ncluster_enabled:{}\r\n\r\n",
            i32::from(replication.cluster().is_some())
        ));
    }

    if wanted("keyspace") {
        out.push_str("# Keyspace\r\n");
        let (keys, expires) = engine.key_counts();
        if keys > 0 {
            // Omitted entirely on an empty keyspace, exactly as real Redis does -- tooling
            // treats the absence of a `db0:` line as "this database is empty".
            out.push_str(&format!("db0:keys={keys},expires={expires},avg_ttl=0\r\n"));
        }
        out.push_str("\r\n");
    }

    out
}

/// Returns `Some(reply)` if `frame` was `INFO`. Lives here rather than in `dispatch` because it
/// reads the `AofWriter` and the `ReplicationHandle`, which plain `dispatch` has no parameter
/// for -- the same reason `SAVE`, `REPLICAOF`, and `CLUSTER` are intercepted.
fn handle_info(
    frame: &Frame,
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"INFO") {
        return None;
    }
    let section = match items.get(1) {
        Some(Frame::Bulk(raw)) => {
            let requested = String::from_utf8_lossy(raw).to_ascii_lowercase();
            match requested.as_str() {
                "all" | "default" | "everything" => None,
                _ => Some(requested),
            }
        }
        _ => None,
    };
    Some(Frame::Bulk(Bytes::from(info_text(
        section.as_deref(),
        engine,
        aof,
        replication,
    ))))
}

/// Returns `Some(reply)` if `frame` was `HELLO`. Moved out of `dispatch` this sprint for one
/// reason: the reply's `role` field must reflect whether this node is a follower, and only
/// `dispatch_and_log` has the `ReplicationHandle` that knows. The protocol-switching behavior is
/// identical to the arm it replaces, and it still mutates the caller's `&mut Protocol`, so
/// `connection.rs`'s `framed.codec_mut().protocol = protocol` keeps working unchanged.
///
/// `dispatch` therefore answers `HELLO` with its unknown-command error, which is correct: its
/// only direct callers are `aof::replay` and the follower apply loop, neither of which can ever
/// see a `HELLO`.
fn handle_hello(
    frame: &Frame,
    protocol: &mut Protocol,
    client_id: u64,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"HELLO") {
        return None;
    }
    let role = if replication
        .is_replica
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        "slave"
    } else {
        "master"
    };
    // Kept consistent with `INFO server`'s `redis_mode`, which reports the same thing.
    let mode = if replication.cluster().is_some() {
        "cluster"
    } else {
        "standalone"
    };
    let args = &items[1..];
    Some(match args.first() {
        None => hello_reply(*protocol, client_id, role, mode),
        Some(Frame::Bulk(arg)) => match arg.as_ref() {
            b"2" => {
                if args.len() > 1 {
                    return Some(Frame::Error("ERR syntax error".into()));
                }
                *protocol = Protocol::Resp2;
                hello_reply(*protocol, client_id, role, mode)
            }
            b"3" => {
                if args.len() > 1 {
                    return Some(Frame::Error("ERR syntax error".into()));
                }
                *protocol = Protocol::Resp3;
                hello_reply(*protocol, client_id, role, mode)
            }
            _ => Frame::Error("NOPROTO unsupported protocol version".into()),
        },
        // A non-Bulk argument was previously caught by `dispatch`'s `frame_to_args`; keep that
        // exact error so the move changes no observable behavior.
        Some(_) => Frame::Error("ERR invalid request, expected array of bulk strings".into()),
    })
}

/// Renders one slow-log entry's argument array. The entry carries only the command name and its
/// first argument, so anything beyond that is summarised with real Redis's own truncation
/// marker -- a shape real Redis itself emits (it truncates at 31 arguments, reserving the 32nd
/// slot for the truncation marker itself), so tooling parses it without special-casing.
fn slowlog_args_frame(entry: &crate::slowlog::SlowLogEntry) -> Frame {
    let mut args = vec![Frame::Bulk(Bytes::from(entry.command.clone()))];
    let shown = usize::from(entry.key.is_some());
    if let Some(key) = &entry.key {
        args.push(Frame::Bulk(key.clone()));
    }
    if entry.arg_count > shown {
        args.push(Frame::Bulk(Bytes::from(format!(
            "... ({} more arguments)",
            entry.arg_count - shown
        ))));
    }
    Frame::Array(args)
}

/// Returns `Some(reply)` if `frame` was `SLOWLOG`. Intercepted here, like `CLUSTER` and `INFO`,
/// because the ring buffer lives on `ReplicationHandle`, which plain `dispatch` cannot see.
///
/// Three subcommands only: `GET [count]`, `LEN`, `RESET`. `SLOWLOG HELP` is out of scope for the
/// same reason `CLUSTER SLOTS` is -- nothing in this repo consumes it.
fn handle_slowlog(
    frame: &Frame,
    replication: &crate::replication::ReplicationHandle,
) -> Option<Frame> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    if !name.eq_ignore_ascii_case(b"SLOWLOG") {
        return None;
    }
    let Some(Frame::Bulk(sub_bytes)) = items.get(1) else {
        return Some(Frame::Error(
            "ERR wrong number of arguments for 'slowlog' command".into(),
        ));
    };
    let sub = String::from_utf8_lossy(sub_bytes).to_ascii_uppercase();
    Some(match sub.as_str() {
        "GET" => {
            // Default 10, matching real Redis. A negative count means "everything", also
            // matching real Redis; anything unparseable is an error rather than a silent 10.
            let count = match items.get(2) {
                None => 10usize,
                Some(Frame::Bulk(raw)) => match std::str::from_utf8(raw)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    Some(n) if n < 0 => crate::slowlog::SLOWLOG_CAPACITY,
                    Some(n) => n as usize,
                    None => {
                        return Some(Frame::Error(
                            "ERR value is not an integer or out of range".into(),
                        ))
                    }
                },
                Some(_) => {
                    return Some(Frame::Error(
                        "ERR value is not an integer or out of range".into(),
                    ))
                }
            };
            Frame::Array(
                replication
                    .slowlog
                    .get(count)
                    .iter()
                    .map(|entry| {
                        Frame::Array(vec![
                            Frame::Integer(entry.id as i64),
                            Frame::Integer(entry.unix_time_secs),
                            Frame::Integer(entry.duration_micros),
                            slowlog_args_frame(entry),
                        ])
                    })
                    .collect(),
            )
        }
        "LEN" => Frame::Integer(replication.slowlog.len() as i64),
        "RESET" => {
            replication.slowlog.reset();
            Frame::Simple("OK".into())
        }
        _ => Frame::Error(format!("ERR unknown SLOWLOG subcommand '{sub}'")),
    })
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
        Ok(()) => {
            replication.record_save();
            Frame::Simple("OK".into())
        }
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

/// The uppercased command name, or `None` for a frame that isn't a command array. Cheap enough to
/// call once per command -- uppercases into a stack buffer rather than allocating.
fn command_name_upper(frame: &Frame) -> Option<CommandName> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let Some(Frame::Bulk(name)) = items.first() else {
        return None;
    };
    upper_name(name)
}

/// The command's first argument (cloned -- one `Bytes` refcount bump, no data copy) and how many
/// arguments followed the name. Read before `frame` is moved into `dispatch_and_log_inner`,
/// because `dispatch` consumes the frame; see this plan's Global Constraints for why the slow log
/// carries this instead of the whole argument list.
fn command_key_and_arity(frame: &Frame) -> (Option<Bytes>, usize) {
    let Frame::Array(items) = frame else {
        return (None, 0);
    };
    let key = match items.get(1) {
        Some(Frame::Bulk(b)) => Some(b.clone()),
        _ => None,
    };
    (key, items.len().saturating_sub(1))
}

/// The `cmd` label value for a command name: its lowercase form if we know the command, the
/// literal `other` otherwise. The `other` fallback is what bounds Prometheus label cardinality --
/// without it, a client sending random command names could create unbounded series.
fn metric_label(name: &str) -> String {
    if KNOWN_COMMANDS.binary_search(&name).is_ok() {
        name.to_ascii_lowercase()
    } else {
        "other".to_string()
    }
}

/// Times and counts every client command, then delegates to `dispatch_and_log_inner`, which
/// holds all the actual behavior. The split exists because the inner function has seven early
/// returns (-MOVED, -CROSSSLOT, -READONLY, SAVE, REPLICAOF, CLUSTER, and the unknown-command
/// fall-through) and instrumenting each one would guarantee a future eighth is missed.
///
/// `dispatch` itself is deliberately *not* instrumented: it is what `aof::replay` and the
/// follower apply loop call, and counting a 5,000-frame boot-time replay as 5,000 client
/// commands would make every dashboard lie about traffic.
///
/// The signature is byte-for-byte the one Sprint 5 left, so none of the ~36 call sites change.
pub fn dispatch_and_log(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    let name = command_name_upper(&frame); // read before `frame` is moved into the inner call
    let name = name.as_ref().map(|n| n.as_str()).unwrap_or("");
    let (first_key, arg_count) = command_key_and_arity(&frame);
    let label = metric_label(name);
    let started = std::time::Instant::now();

    let reply = dispatch_and_log_inner(engine, aof, replication, frame, protocol, client_id);

    let elapsed = started.elapsed();
    replication.command_executed();
    ::metrics::counter!("rocket_mem_commands_total", "cmd" => label.clone()).increment(1);
    ::metrics::histogram!("rocket_mem_command_duration_seconds", "cmd" => label.clone())
        .record(elapsed.as_secs_f64());
    if matches!(reply, Frame::Error(_)) {
        ::metrics::counter!("rocket_mem_command_errors_total", "cmd" => label).increment(1);
    }
    replication
        .slowlog
        .maybe_record(name, first_key, arg_count, elapsed);
    reply
}

/// Wraps `dispatch`, additionally appending successful write commands to `aof`. `dispatch`
/// itself is never modified — see ../../docs/superpowers/specs/2026-08-30-sprint-4-spec.md
/// for why AOF logging lives here instead of inside dispatch's own match arms.
fn dispatch_and_log_inner(
    engine: &Engine,
    aof: &crate::aof::AofWriter,
    replication: &crate::replication::ReplicationHandle,
    frame: Frame,
    protocol: &mut Protocol,
    client_id: u64,
) -> Frame {
    // Checked before everything else, including the -READONLY gate below: a redirect says which
    // node should handle this key at all, and it must land before any lock is taken or any
    // interception runs. See ../../docs/superpowers/specs/2026-08-30-sprint-6-spec.md for the
    // MOVED-beats-READONLY precedence argument.
    if let Some(redirect) = cluster_redirect(&frame, replication) {
        return redirect;
    }

    // Checked before the SAVE/REPLICAOF interceptions below, and immediately after the
    // cluster redirect above (both interceptions are no-ops against WRITE_COMMANDS so
    // ordering relative to them doesn't matter) and extract_write_command_name's own later
    // call further down (so a rejected write never touches the AOF ordering lock).
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
    if let Some(reply) = handle_cluster(&frame, replication) {
        return reply;
    }
    if let Some(reply) = handle_info(&frame, engine, aof, replication) {
        return reply;
    }
    if let Some(reply) = handle_hello(&frame, protocol, client_id, replication) {
        return reply;
    }
    if let Some(reply) = handle_slowlog(&frame, replication) {
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
    fn metric_label_lowercases_known_commands_and_collapses_the_rest() {
        assert_eq!(metric_label("GET"), "get");
        assert_eq!(metric_label("ZINCRBY"), "zincrby");
        assert_eq!(metric_label("CLUSTER"), "cluster");
        // an unknown name must never become its own Prometheus series
        assert_eq!(metric_label("DEFINITELYNOTACOMMAND"), "other");
        assert_eq!(metric_label(""), "other");
    }

    #[test]
    fn command_name_upper_reads_the_command_name_from_any_frame_shape() {
        assert_eq!(
            command_name_upper(&cmd(&[b"get", b"k"])).unwrap().as_str(),
            "GET"
        );
        assert_eq!(
            command_name_upper(&cmd(&[b"SeT", b"k", b"v"]))
                .unwrap()
                .as_str(),
            "SET"
        );
        assert!(command_name_upper(&Frame::Simple("nope".into())).is_none());
        assert!(command_name_upper(&Frame::Array(vec![])).is_none());
    }

    #[test]
    fn upper_name_uppercases_ascii_into_a_stack_buffer() {
        assert_eq!(upper_name(b"get").unwrap().as_str(), "GET");
        assert_eq!(upper_name(b"SeT").unwrap().as_str(), "SET");
        assert_eq!(upper_name(b"ZINCRBY").unwrap().as_str(), "ZINCRBY");
        assert_eq!(upper_name(b"").unwrap().as_str(), "");
    }

    #[test]
    fn upper_name_rejects_names_that_cannot_be_a_command() {
        // longer than any real command name -- necessarily unknown, and handled on the cold path
        assert!(upper_name(&[b'a'; MAX_COMMAND_NAME_LEN + 1]).is_none());
        // non-ASCII cannot be uppercased byte-wise, and no command name contains it
        assert!(upper_name(&[0xff, 0xfe]).is_none());
    }

    #[test]
    fn an_over_long_command_name_still_gets_the_normal_unknown_command_error() {
        let engine = Engine::new();
        let long_name = vec![b'A'; MAX_COMMAND_NAME_LEN + 1];
        let reply = dispatch(
            &engine,
            Frame::Array(vec![Frame::Bulk(Bytes::from(long_name.clone()))]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error(format!(
                "ERR unknown command '{}'",
                String::from_utf8_lossy(&long_name)
            ))
        );
    }

    /// A handle whose slow-log threshold is 1ns, so every command qualifies. Nothing else about
    /// it differs from `ReplicationHandle::default()`.
    fn slowlog_handle() -> ReplicationHandle {
        ReplicationHandle::default().with_slowlog_threshold(std::time::Duration::from_nanos(1))
    }

    #[test]
    fn command_key_and_arity_reads_the_first_argument_and_the_count() {
        assert_eq!(
            command_key_and_arity(&cmd(&[b"SET", b"k", b"v"])),
            (Some(Bytes::from_static(b"k")), 2)
        );
        assert_eq!(command_key_and_arity(&cmd(&[b"PING"])), (None, 0));
        assert_eq!(
            command_key_and_arity(&cmd(&[b"LRANGE", b"mylist", b"0", b"-1"])),
            (Some(Bytes::from_static(b"mylist")), 3)
        );
        assert_eq!(command_key_and_arity(&Frame::Simple("x".into())), (None, 0));
    }

    #[test]
    fn a_slow_command_is_recorded_with_its_name_key_and_arity() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        let entries = replication.slowlog.get(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].command, "SET");
        assert_eq!(entries[0].key, Some(Bytes::from_static(b"k")));
        assert_eq!(entries[0].arg_count, 2);
    }

    #[test]
    fn a_fast_command_is_not_recorded_at_the_default_threshold() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default(); // 10ms threshold
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PING"]),
            &mut Protocol::default(),
            1,
        );
        assert!(replication.slowlog.is_empty());
    }

    #[test]
    fn slowlog_len_counts_recorded_entries() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SET", b"k", b"v"]),
            &mut Protocol::default(),
            1,
        );
        // the SLOWLOG LEN command is itself recorded only *after* its reply is built, so it
        // reports the one SET that preceded it
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"SLOWLOG", b"LEN"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Integer(1)
        );
    }

    #[test]
    fn slowlog_get_returns_id_timestamp_duration_and_arguments() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"LRANGE", b"mylist", b"0", b"-1"]),
            &mut Protocol::default(),
            1,
        );

        let Frame::Array(entries) = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SLOWLOG", b"GET"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(entries.len(), 1);
        let Frame::Array(entry) = &entries[0] else {
            panic!("expected each entry to be an Array")
        };
        assert_eq!(entry.len(), 4);
        assert_eq!(entry[0], Frame::Integer(0)); // id
        let Frame::Integer(timestamp) = entry[1] else {
            panic!("expected an integer timestamp")
        };
        assert!(timestamp > 1_700_000_000);
        assert!(matches!(entry[2], Frame::Integer(micros) if micros >= 0));
        assert_eq!(
            entry[3],
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"LRANGE")),
                Frame::Bulk(Bytes::from_static(b"mylist")),
                // real Redis's own truncation marker, for the arguments the entry doesn't carry
                Frame::Bulk(Bytes::from_static(b"... (2 more arguments)")),
            ])
        );
    }

    #[test]
    fn slowlog_get_honours_an_explicit_count() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        for _ in 0..3 {
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"PING"]),
                &mut Protocol::default(),
                1,
            );
        }
        let Frame::Array(entries) = dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"SLOWLOG", b"GET", b"2"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn slowlog_reset_replies_ok_and_empties_the_buffer() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = slowlog_handle();
        dispatch_and_log(
            &engine,
            &aof,
            &replication,
            cmd(&[b"PING"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(replication.slowlog.len(), 1);
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"SLOWLOG", b"RESET"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        // RESET emptied the buffer; the wrapper then recorded the RESET itself, so exactly one
        // entry remains -- and it is the RESET, not the PING.
        assert_eq!(replication.slowlog.len(), 1);
        assert_eq!(replication.slowlog.get(1)[0].command, "SLOWLOG");
    }

    #[test]
    fn an_unknown_slowlog_subcommand_is_an_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"SLOWLOG", b"HELP"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown SLOWLOG subcommand 'HELP'".into())
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"SLOWLOG"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'slowlog' command".into())
        );
    }

    #[test]
    fn dispatch_and_log_counts_every_command_it_handles() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        for command in [
            cmd(&[b"SET", b"k", b"v"]),
            cmd(&[b"GET", b"k"]),
            cmd(&[b"PING"]),
        ] {
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                command,
                &mut Protocol::default(),
                1,
            );
        }
        assert_eq!(replication.total_commands(), 3);
    }

    #[test]
    fn dispatch_and_log_still_behaves_identically_after_the_wrapper_split() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"SET", b"k", b"v"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Simple("OK".into())
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"GET", b"k"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"v"))
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"NOPE"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown command 'NOPE'".into())
        );
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
        let (_dir, aof) = test_aof();
        let Frame::Bulk(info) = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"INFO"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        assert!(!info.is_empty());
    }

    fn info_text_for(replication: &ReplicationHandle, engine: &Engine, args: &[&[u8]]) -> String {
        let (_dir, aof) = test_aof();
        let mut command = vec![&b"INFO"[..]];
        command.extend_from_slice(args);
        let Frame::Bulk(text) = dispatch_and_log(
            engine,
            &aof,
            replication,
            cmd(&command),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("INFO should reply with a Bulk string")
        };
        String::from_utf8(text.to_vec()).unwrap()
    }

    #[test]
    fn info_emits_every_section_by_default() {
        let engine = Engine::new();
        let text = info_text_for(&ReplicationHandle::default(), &engine, &[]);
        for header in [
            "# Server",
            "# Clients",
            "# Memory",
            "# Persistence",
            "# Stats",
            "# Replication",
            "# Cluster",
        ] {
            assert!(text.contains(header), "missing {header} in:\n{text}");
        }
        assert!(text.contains("redis_version:rocket-mem-"), "{text}");
        assert!(text.contains("redis_mode:standalone\r\n"), "{text}");
        assert!(text.contains("maxmemory_policy:allkeys-lru\r\n"), "{text}");
        assert!(text.contains("aof_enabled:1\r\n"), "{text}");
        assert!(text.contains("rdb_bgsave_in_progress:0\r\n"), "{text}");
        assert!(text.contains("aof_fsync_policy:no\r\n"), "{text}"); // test_aof uses Never
    }

    #[test]
    fn info_with_a_section_argument_returns_only_that_section() {
        let engine = Engine::new();
        let text = info_text_for(&ReplicationHandle::default(), &engine, &[b"replication"]);
        assert!(text.contains("# Replication"), "{text}");
        assert!(text.contains("role:master\r\n"), "{text}");
        assert!(!text.contains("# Memory"), "{text}");
        // the section name is case-insensitive, like real Redis
        let upper = info_text_for(&ReplicationHandle::default(), &engine, &[b"REPLICATION"]);
        assert!(upper.contains("# Replication"), "{upper}");
        // `all` and `default` both mean everything
        let all = info_text_for(&ReplicationHandle::default(), &engine, &[b"all"]);
        assert!(
            all.contains("# Memory") && all.contains("# Replication"),
            "{all}"
        );
    }

    #[test]
    fn info_reports_role_slave_on_a_replica() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        replication
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let text = info_text_for(&replication, &engine, &[b"replication"]);
        assert!(text.contains("role:slave\r\n"), "{text}");
        assert!(text.contains("master_link_status:down\r\n"), "{text}");
        assert!(!text.contains("connected_slaves:"), "{text}");
    }

    #[test]
    fn info_reports_connected_slaves_on_a_master() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        replication.registry.register(tx);
        let text = info_text_for(&replication, &engine, &[b"replication"]);
        assert!(text.contains("role:master\r\n"), "{text}");
        assert!(text.contains("connected_slaves:1\r\n"), "{text}");
        assert!(!text.contains("master_host:"), "{text}");
    }

    #[test]
    fn info_keyspace_line_appears_only_when_there_are_keys() {
        let engine = Engine::new();
        let empty = info_text_for(&ReplicationHandle::default(), &engine, &[b"keyspace"]);
        assert!(!empty.contains("db0:"), "{empty}");

        engine.set(
            Bytes::from_static(b"a"),
            Value::String(Bytes::from_static(b"1")),
        );
        engine.set(
            Bytes::from_static(b"b"),
            Value::String(Bytes::from_static(b"2")),
        );
        engine.expire_at(
            b"b",
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        );
        let filled = info_text_for(&ReplicationHandle::default(), &engine, &[b"keyspace"]);
        assert!(
            filled.contains("db0:keys=2,expires=1,avg_ttl=0\r\n"),
            "{filled}"
        );
    }

    #[test]
    fn info_reports_cluster_mode_from_the_loaded_config() {
        let engine = Engine::new();
        let off = info_text_for(&ReplicationHandle::default(), &engine, &[b"cluster"]);
        assert!(off.contains("cluster_enabled:0\r\n"), "{off}");
        let on = info_text_for(&cluster_handle("shard-a"), &engine, &[b"cluster"]);
        assert!(on.contains("cluster_enabled:1\r\n"), "{on}");
        let server = info_text_for(&cluster_handle("shard-a"), &engine, &[b"server"]);
        assert!(server.contains("redis_mode:cluster\r\n"), "{server}");
    }

    #[test]
    fn info_stats_counts_the_commands_that_ran_before_it() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let replication = ReplicationHandle::default();
        replication.connection_opened();
        for _ in 0..3 {
            dispatch_and_log(
                &engine,
                &aof,
                &replication,
                cmd(&[b"PING"]),
                &mut Protocol::default(),
                1,
            );
        }
        let text = info_text_for(&replication, &engine, &[b"stats"]);
        // 3 PINGs; the INFO itself is counted by the wrapper only *after* the body ran
        assert!(text.contains("total_commands_processed:3\r\n"), "{text}");
        assert!(text.contains("total_connections_received:1\r\n"), "{text}");
        assert!(text.contains("expired_keys:0\r\n"), "{text}");
        assert!(text.contains("evicted_keys:0\r\n"), "{text}");
        let clients = info_text_for(&replication, &engine, &[b"clients"]);
        assert!(clients.contains("connected_clients:1\r\n"), "{clients}");
    }

    #[test]
    fn info_memory_reports_a_configured_maxmemory() {
        let engine = Engine::with_maxmemory(4_096);
        let text = info_text_for(&ReplicationHandle::default(), &engine, &[b"memory"]);
        assert!(text.contains("maxmemory:4096\r\n"), "{text}");
        assert!(text.contains("used_memory:"), "{text}");
        assert!(text.contains("used_memory_human:"), "{text}");
    }

    #[tokio::test]
    async fn info_reports_the_master_address_while_replicating() {
        let engine = Engine::new();
        let replication = ReplicationHandle::default();
        replication.start_replicating("127.0.0.1:1".to_string()); // nothing listening; fine
        let text = info_text_for(&replication, &engine, &[b"replication"]);
        assert!(text.contains("master_host:127.0.0.1\r\n"), "{text}");
        assert!(text.contains("master_port:1\r\n"), "{text}");
        replication.stop_replicating();
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
        let (_dir, aof) = test_aof();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"HELLO"]),
            &mut protocol,
            7,
        );
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
        let (_dir, aof) = test_aof();
        let mut protocol = Protocol::Resp3;
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"HELLO", b"2"]),
            &mut protocol,
            1,
        );
        assert_eq!(protocol, Protocol::Resp2);
        let Frame::Map(pairs) = reply else {
            panic!("expected Map")
        };
        assert!(pairs.contains(&(Frame::Bulk(Bytes::from_static(b"proto")), Frame::Integer(2))));
    }

    #[test]
    fn hello_3_switches_protocol_to_resp3() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"HELLO", b"3"]),
            &mut protocol,
            42,
        );
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
        let (_dir, aof) = test_aof();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"HELLO", b"4"]),
            &mut protocol,
            1,
        );
        assert_eq!(protocol, Protocol::Resp2); // unchanged
        assert_eq!(
            reply,
            Frame::Error("NOPROTO unsupported protocol version".into())
        );
    }

    #[test]
    fn hello_reports_role_slave_on_a_replica_and_master_otherwise() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();

        let master = ReplicationHandle::default();
        let Frame::Map(fields) = dispatch_and_log(
            &engine,
            &aof,
            &master,
            cmd(&[b"HELLO"]),
            &mut Protocol::default(),
            7,
        ) else {
            panic!("expected Map")
        };
        assert!(fields.contains(&(
            Frame::Bulk(Bytes::from_static(b"role")),
            Frame::Bulk(Bytes::from_static(b"master"))
        )));

        let replica = ReplicationHandle::default();
        replica
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let Frame::Map(fields) = dispatch_and_log(
            &engine,
            &aof,
            &replica,
            cmd(&[b"HELLO"]),
            &mut Protocol::default(),
            7,
        ) else {
            panic!("expected Map")
        };
        assert!(
            fields.contains(&(
                Frame::Bulk(Bytes::from_static(b"role")),
                Frame::Bulk(Bytes::from_static(b"slave"))
            )),
            "{fields:?}"
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
    fn sscan_returns_all_members_in_one_call_with_a_done_cursor() {
        let engine = Engine::new();
        dispatch(
            &engine,
            cmd(&[b"SADD", b"myset", b"a", b"b"]),
            &mut Protocol::default(),
            1,
        );
        let reply = dispatch(
            &engine,
            cmd(&[b"SSCAN", b"myset", b"0"]),
            &mut Protocol::default(),
            1,
        );
        let Frame::Array(parts) = reply else {
            panic!("expected Array")
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], Frame::Bulk(Bytes::from_static(b"0")));
        let Frame::Array(members) = &parts[1] else {
            panic!("expected Array of members")
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn sscan_on_missing_key_returns_an_empty_array_not_an_error() {
        let engine = Engine::new();
        let reply = dispatch(
            &engine,
            cmd(&[b"SSCAN", b"missing", b"0"]),
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
    fn sscan_with_a_non_numeric_cursor_is_a_resp_error() {
        let engine = Engine::new();
        assert_eq!(
            dispatch(
                &engine,
                cmd(&[b"SSCAN", b"myset", b"notacursor"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR invalid cursor".into())
        );
    }

    #[test]
    fn sscan_on_a_string_key_returns_wrongtype() {
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
                cmd(&[b"SSCAN", b"k", b"0"]),
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
        let (_dir, aof) = test_aof();
        let mut protocol = Protocol::Resp2;
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
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

    /// A three-shard topology whose ranges are the even thirds of the slot space, with this
    /// process being `node_id`. Uses `ReplicationHandle::default()` (its own throwaway Engine
    /// and the `./dump.snapshot` path) because none of these tests issue a SAVE.
    fn cluster_handle(node_id: &str) -> ReplicationHandle {
        let config = crate::cluster::ClusterConfig::parse(
            "shard-a 127.0.0.1:7001 0 5460\n\
             shard-b 127.0.0.1:7002 5461 10922\n\
             shard-c 127.0.0.1:7003 10923 16383\n",
            node_id,
        )
        .unwrap();
        ReplicationHandle::default().with_cluster(std::sync::Arc::new(config))
    }

    #[test]
    fn cluster_keyslot_answers_the_reference_slot_even_with_cluster_mode_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"CLUSTER", b"KEYSLOT", b"foo"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Integer(12182));
    }

    #[test]
    fn cluster_keyslot_honours_hash_tags() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"KEYSLOT", b"{user1000}.following"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Integer(3443));
    }

    #[test]
    fn cluster_keyslot_with_wrong_arity_is_an_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"KEYSLOT"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error("ERR wrong number of arguments for 'cluster|keyslot' command".into())
        );
    }

    #[test]
    fn cluster_myid_returns_this_nodes_id_or_a_zero_id_when_disabled() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &cluster_handle("shard-b"),
                cmd(&[b"CLUSTER", b"MYID"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b"shard-b"))
        );
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"CLUSTER", b"MYID"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from("0".repeat(40)))
        );
    }

    #[test]
    fn cluster_info_reports_enabled_and_the_node_count() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"INFO"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert!(text.contains("cluster_enabled:1\r\n"), "{text}");
        assert!(text.contains("cluster_state:ok\r\n"), "{text}");
        assert!(text.contains("cluster_slots_assigned:16384\r\n"), "{text}");
        assert!(text.contains("cluster_known_nodes:3\r\n"), "{text}");
        assert!(text.contains("cluster_size:3\r\n"), "{text}");
    }

    #[test]
    fn cluster_info_reports_disabled_when_no_config_was_loaded() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"CLUSTER", b"INFO"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        assert!(text.contains("cluster_enabled:0\r\n"), "{text}");
        assert!(text.contains("cluster_known_nodes:0\r\n"), "{text}");
    }

    #[test]
    fn cluster_nodes_lists_every_node_with_myself_flagged() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Bulk(text) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-b"),
            cmd(&[b"CLUSTER", b"NODES"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Bulk")
        };
        let text = String::from_utf8(text.to_vec()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(
            lines[0],
            "shard-a 127.0.0.1:7001@17001 master - 0 0 0 connected 0-5460"
        );
        assert_eq!(
            lines[1],
            "shard-b 127.0.0.1:7002@17002 myself,master - 0 0 0 connected 5461-10922"
        );
        assert_eq!(
            lines[2],
            "shard-c 127.0.0.1:7003@17003 master - 0 0 0 connected 10923-16383"
        );
    }

    #[test]
    fn cluster_nodes_is_empty_when_cluster_mode_is_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"CLUSTER", b"NODES"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Bulk(Bytes::from_static(b""))
        );
    }

    #[test]
    fn cluster_shards_describes_every_shards_slots_and_its_one_node() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let Frame::Array(shards) = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"CLUSTER", b"SHARDS"]),
            &mut Protocol::default(),
            1,
        ) else {
            panic!("expected Array")
        };
        assert_eq!(shards.len(), 3);
        let Frame::Array(first) = &shards[0] else {
            panic!("expected each shard to be an Array of alternating key/value frames")
        };
        assert_eq!(first[0], Frame::Bulk(Bytes::from_static(b"slots")));
        assert_eq!(
            first[1],
            Frame::Array(vec![Frame::Integer(0), Frame::Integer(5460)])
        );
        assert_eq!(first[2], Frame::Bulk(Bytes::from_static(b"nodes")));
        let Frame::Array(nodes) = &first[3] else {
            panic!("expected a nodes array")
        };
        assert_eq!(nodes.len(), 1, "a shard has exactly one node this sprint");
        let Frame::Array(node) = &nodes[0] else {
            panic!("expected the node to be an Array of alternating key/value frames")
        };
        assert_eq!(node[0], Frame::Bulk(Bytes::from_static(b"id")));
        assert_eq!(node[1], Frame::Bulk(Bytes::from_static(b"shard-a")));
        assert_eq!(node[2], Frame::Bulk(Bytes::from_static(b"port")));
        assert_eq!(node[3], Frame::Integer(7001));
        assert_eq!(node[4], Frame::Bulk(Bytes::from_static(b"ip")));
        assert_eq!(node[5], Frame::Bulk(Bytes::from_static(b"127.0.0.1")));
        assert_eq!(node[6], Frame::Bulk(Bytes::from_static(b"endpoint")));
        assert_eq!(node[7], Frame::Bulk(Bytes::from_static(b"127.0.0.1")));
        assert_eq!(node[8], Frame::Bulk(Bytes::from_static(b"role")));
        assert_eq!(node[9], Frame::Bulk(Bytes::from_static(b"master")));
        assert_eq!(
            node[10],
            Frame::Bulk(Bytes::from_static(b"replication-offset"))
        );
        assert_eq!(node[11], Frame::Integer(0));
        assert_eq!(node[12], Frame::Bulk(Bytes::from_static(b"health")));
        assert_eq!(node[13], Frame::Bulk(Bytes::from_static(b"online")));
    }

    #[test]
    fn cluster_shards_is_empty_when_cluster_mode_is_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &ReplicationHandle::default(),
                cmd(&[b"CLUSTER", b"SHARDS"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Array(vec![])
        );
    }

    #[test]
    fn an_unknown_cluster_subcommand_is_an_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &cluster_handle("shard-a"),
                cmd(&[b"CLUSTER", b"RESHARD"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR unknown CLUSTER subcommand 'RESHARD'".into())
        );
    }

    #[test]
    fn cluster_with_no_subcommand_is_an_arity_error() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &cluster_handle("shard-a"),
                cmd(&[b"CLUSTER"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("ERR wrong number of arguments for 'cluster' command".into())
        );
    }

    #[test]
    fn a_key_this_node_owns_is_served_normally() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // "hello" hashes to slot 866, which shard-a owns
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"SET", b"hello", b"world"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    #[test]
    fn a_key_this_node_does_not_own_is_redirected_with_moved() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // "foo" hashes to slot 12182, which shard-c owns
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"GET", b"foo"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Error("MOVED 12182 127.0.0.1:7003".into()));
    }

    #[test]
    fn a_redirected_write_never_reaches_the_engine() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"SET", b"foo", b"bar"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Error("MOVED 12182 127.0.0.1:7003".into()));
        assert_eq!(engine.get(b"foo"), None); // nothing was written
    }

    #[test]
    fn keys_spanning_two_slots_are_rejected_with_crossslot() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // "hello" is slot 866, "foo" is slot 12182
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[b"MSET", b"hello", b"1", b"foo", b"2"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(
            reply,
            Frame::Error("CROSSSLOT Keys in request don't hash to the same slot".into())
        );
        assert_eq!(engine.get(b"hello"), None);
    }

    #[test]
    fn a_hash_tag_keeps_a_multi_key_command_on_one_slot() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        // both keys hash on "user1000" => slot 3443, owned by shard-a
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &cluster_handle("shard-a"),
            cmd(&[
                b"MSET",
                b"{user1000}.name",
                b"ada",
                b"{user1000}.city",
                b"london",
            ]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
        assert_eq!(
            engine.get(b"{user1000}.city"),
            Some(engine::Value::String(Bytes::from_static(b"london")))
        );
    }

    #[test]
    fn keyless_commands_are_never_redirected() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let handle = cluster_handle("shard-a");
        for (command, expected) in [
            (cmd(&[b"PING"]), Frame::Simple("PONG".into())),
            (cmd(&[b"SELECT", b"0"]), Frame::Simple("OK".into())),
            (
                cmd(&[b"CLUSTER", b"KEYSLOT", b"foo"]),
                Frame::Integer(12182),
            ),
        ] {
            assert_eq!(
                dispatch_and_log(&engine, &aof, &handle, command, &mut Protocol::default(), 1),
                expected
            );
        }
    }

    #[test]
    fn nothing_is_redirected_when_cluster_mode_is_off() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let reply = dispatch_and_log(
            &engine,
            &aof,
            &ReplicationHandle::default(),
            cmd(&[b"MSET", b"hello", b"1", b"foo", b"2"]),
            &mut Protocol::default(),
            1,
        );
        assert_eq!(reply, Frame::Simple("OK".into()));
    }

    #[test]
    fn moved_takes_precedence_over_readonly_on_a_node_that_is_both() {
        let engine = Engine::new();
        let (_dir, aof) = test_aof();
        let handle = cluster_handle("shard-a");
        handle
            .is_replica
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // A write to a key this node doesn't own, on a node that is also a read-only follower.
        // MOVED wins: READONLY would send a cluster-aware client into a retry loop against a
        // node that will never accept this key, while MOVED sends it to the owner, where a
        // READONLY (if that node is also a follower) is actionable.
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &handle,
                cmd(&[b"SET", b"foo", b"bar"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("MOVED 12182 127.0.0.1:7003".into())
        );
        // ...and a write to a key it DOES own still gets the READONLY it deserves
        assert_eq!(
            dispatch_and_log(
                &engine,
                &aof,
                &handle,
                cmd(&[b"SET", b"hello", b"world"]),
                &mut Protocol::default(),
                1
            ),
            Frame::Error("READONLY You can't write against a read only replica.".into())
        );
    }
}
