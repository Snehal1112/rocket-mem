use crate::Frame;
use bytes::{Buf, BufMut, BytesMut};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

pub struct RespCodec;

impl Encoder<Frame> for RespCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            Frame::Simple(s) => {
                dst.put_u8(b'+');
                dst.put_slice(s.as_bytes());
                dst.put_slice(b"\r\n");
            }
            Frame::Error(s) => {
                dst.put_u8(b'-');
                dst.put_slice(s.as_bytes());
                dst.put_slice(b"\r\n");
            }
            Frame::Integer(n) => {
                dst.put_u8(b':');
                dst.put_slice(n.to_string().as_bytes());
                dst.put_slice(b"\r\n");
            }
            Frame::Bulk(b) => {
                dst.put_u8(b'$');
                dst.put_slice(b.len().to_string().as_bytes());
                dst.put_slice(b"\r\n");
                dst.put_slice(&b);
                dst.put_slice(b"\r\n");
            }
            Frame::Null => {
                dst.put_slice(b"$-1\r\n");
            }
            Frame::Array(items) => {
                dst.put_u8(b'*');
                dst.put_slice(items.len().to_string().as_bytes());
                dst.put_slice(b"\r\n");
                for item in items {
                    self.encode(item, dst)?;
                }
            }
        }
        Ok(())
    }
}

/// Finds the index of the `\r` in the first `\r\n` in `buf`, if a complete one exists.
fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

/// Parses one complete frame starting at the front of `buf`.
/// Returns `Ok(None)` if `buf` doesn't yet contain a complete frame (caller should wait
/// for more bytes) and never consumes anything in that case. Returns `Ok(Some((frame,
/// consumed)))` on success, where `consumed` is the exact byte count to drop from `buf`.
fn parse_frame(buf: &[u8]) -> io::Result<Option<(Frame, usize)>> {
    if buf.is_empty() {
        return Ok(None);
    }
    match buf[0] {
        b'+' | b'-' | b':' => {
            let Some(crlf) = find_crlf(&buf[1..]) else {
                return Ok(None);
            };
            let line = std::str::from_utf8(&buf[1..1 + crlf])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let consumed = 1 + crlf + 2;
            let frame = match buf[0] {
                b'+' => Frame::Simple(line.to_string()),
                b'-' => Frame::Error(line.to_string()),
                b':' => Frame::Integer(
                    line.parse()
                        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad integer"))?,
                ),
                _ => unreachable!(),
            };
            Ok(Some((frame, consumed)))
        }
        b'$' => {
            let Some(crlf) = find_crlf(&buf[1..]) else {
                return Ok(None);
            };
            let len_str = std::str::from_utf8(&buf[1..1 + crlf])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let len: i64 = len_str
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad bulk length"))?;
            let header_len = 1 + crlf + 2;
            if len == -1 {
                return Ok(Some((Frame::Null, header_len)));
            }
            let len = len as usize;
            let total = header_len + len + 2;
            if buf.len() < total {
                return Ok(None);
            }
            let data = &buf[header_len..header_len + len];
            Ok(Some((
                Frame::Bulk(bytes::Bytes::copy_from_slice(data)),
                total,
            )))
        }
        b'*' => {
            let Some(crlf) = find_crlf(&buf[1..]) else {
                return Ok(None);
            };
            let count_str = std::str::from_utf8(&buf[1..1 + crlf])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let count: i64 = count_str
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad array length"))?;
            let mut consumed = 1 + crlf + 2;
            let mut items = Vec::new();
            for _ in 0..count.max(0) {
                match parse_frame(&buf[consumed..])? {
                    Some((item, item_consumed)) => {
                        items.push(item);
                        consumed += item_consumed;
                    }
                    None => return Ok(None),
                }
            }
            Ok(Some((Frame::Array(items), consumed)))
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown RESP type byte: {other:#x}"),
        )),
    }
}

impl Decoder for RespCodec {
    type Item = Frame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, Self::Error> {
        match parse_frame(src)? {
            Some((frame, consumed)) => {
                src.advance(consumed);
                Ok(Some(frame))
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;
    use bytes::{Bytes, BytesMut};
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn encodes_simple_string() {
        let mut buf = BytesMut::new();
        RespCodec
            .encode(Frame::Simple("OK".into()), &mut buf)
            .unwrap();
        assert_eq!(&buf[..], b"+OK\r\n");
    }

    #[test]
    fn encodes_error() {
        let mut buf = BytesMut::new();
        RespCodec
            .encode(Frame::Error("ERR bad".into()), &mut buf)
            .unwrap();
        assert_eq!(&buf[..], b"-ERR bad\r\n");
    }

    #[test]
    fn encodes_integer() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Integer(42), &mut buf).unwrap();
        assert_eq!(&buf[..], b":42\r\n");
    }

    #[test]
    fn encodes_bulk_string() {
        let mut buf = BytesMut::new();
        RespCodec
            .encode(Frame::Bulk(Bytes::from_static(b"hi")), &mut buf)
            .unwrap();
        assert_eq!(&buf[..], b"$2\r\nhi\r\n");
    }

    #[test]
    fn encodes_null_as_resp2_null_bulk_string() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Null, &mut buf).unwrap();
        assert_eq!(&buf[..], b"$-1\r\n");
    }

    #[test]
    fn encodes_array_by_concatenating_each_items_encoding() {
        let mut buf = BytesMut::new();
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"a")),
            Frame::Integer(1),
        ]);
        RespCodec.encode(frame, &mut buf).unwrap();
        assert_eq!(&buf[..], b"*2\r\n$1\r\na\r\n:1\r\n");
    }

    #[test]
    fn decodes_simple_string() {
        let mut buf = BytesMut::from(&b"+OK\r\n"[..]);
        let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame, Frame::Simple("OK".into()));
        assert!(buf.is_empty());
    }

    #[test]
    fn decodes_error() {
        let mut buf = BytesMut::from(&b"-ERR bad\r\n"[..]);
        assert_eq!(
            RespCodec.decode(&mut buf).unwrap().unwrap(),
            Frame::Error("ERR bad".into())
        );
    }

    #[test]
    fn decodes_integer() {
        let mut buf = BytesMut::from(&b":42\r\n"[..]);
        assert_eq!(
            RespCodec.decode(&mut buf).unwrap().unwrap(),
            Frame::Integer(42)
        );
    }

    #[test]
    fn decodes_bulk_string() {
        let mut buf = BytesMut::from(&b"$2\r\nhi\r\n"[..]);
        assert_eq!(
            RespCodec.decode(&mut buf).unwrap().unwrap(),
            Frame::Bulk(Bytes::from_static(b"hi"))
        );
    }

    #[test]
    fn decodes_null_bulk_string() {
        let mut buf = BytesMut::from(&b"$-1\r\n"[..]);
        assert_eq!(RespCodec.decode(&mut buf).unwrap().unwrap(), Frame::Null);
    }

    #[test]
    fn decodes_array_of_bulk_strings_the_shape_a_real_client_sends() {
        let mut buf = BytesMut::from(&b"*3\r\n$3\r\nSET\r\n$3\r\nfoo\r\n$3\r\nbar\r\n"[..]);
        let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(
            frame,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"foo")),
                Frame::Bulk(Bytes::from_static(b"bar")),
            ])
        );
    }

    #[test]
    fn unknown_type_byte_is_an_error_not_a_panic() {
        let mut buf = BytesMut::from(&b"@nope\r\n"[..]);
        assert!(RespCodec.decode(&mut buf).is_err());
    }

    #[test]
    fn empty_buffer_returns_ok_none_not_an_error() {
        let mut buf = BytesMut::new();
        assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
    }

    #[test]
    fn decode_returns_none_on_a_bulk_string_split_mid_header() {
        let mut buf = BytesMut::from(&b"$3\r\nfo"[..]); // header + 2 of 3 body bytes
        assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
        assert_eq!(&buf[..], b"$3\r\nfo"); // nothing consumed on an incomplete frame
    }

    #[test]
    fn decode_reassembles_a_bulk_string_split_across_two_reads() {
        let mut buf = BytesMut::from(&b"$3\r\nfo"[..]);
        assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(b"o\r\n"); // the rest arrives in a second read
        let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(frame, Frame::Bulk(Bytes::from_static(b"foo")));
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_reassembles_a_full_command_split_across_three_reads() {
        // mirrors a real client sending `SET foo bar` split at arbitrary byte boundaries
        let mut buf = BytesMut::from(&b"*3\r\n$3\r\nSET\r\n"[..]);
        assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(b"$3\r\nfo");
        assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
        buf.extend_from_slice(b"o\r\n$3\r\nbar\r\n");
        let frame = RespCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(
            frame,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"foo")),
                Frame::Bulk(Bytes::from_static(b"bar")),
            ])
        );
    }

    #[test]
    fn decode_returns_none_when_only_the_array_header_has_arrived() {
        let mut buf = BytesMut::from(&b"*2\r\n"[..]);
        assert_eq!(RespCodec.decode(&mut buf).unwrap(), None);
        assert_eq!(&buf[..], b"*2\r\n");
    }

    #[test]
    fn decode_only_consumes_one_frame_when_two_full_commands_arrive_pipelined_in_one_read() {
        let mut buf = BytesMut::from(&b"+OK\r\n+ALSO OK\r\n"[..]);
        let first = RespCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(first, Frame::Simple("OK".into()));
        assert_eq!(&buf[..], b"+ALSO OK\r\n"); // second frame untouched, ready for the next decode() call
        let second = RespCodec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(second, Frame::Simple("ALSO OK".into()));
        assert!(buf.is_empty());
    }
}
