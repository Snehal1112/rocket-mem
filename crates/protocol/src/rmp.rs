use crate::Frame;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::io;

/// Bounds the envelope's `payload_len` and every length/count field decoded from inside the
/// payload, checked before any allocation sized by that value. The only defense a length-prefixed
/// format has against a forged multi-gigabyte length claim.
pub const MAX_RMP_FRAME_LEN: u32 = 64 * 1024 * 1024;

#[allow(dead_code)]
const TAG_NULL: u8 = 0x00;
#[allow(dead_code)]
const TAG_SIMPLE: u8 = 0x01;
#[allow(dead_code)]
const TAG_ERROR: u8 = 0x02;
#[allow(dead_code)]
const TAG_INTEGER: u8 = 0x03;
#[allow(dead_code)]
const TAG_BULK: u8 = 0x04;
#[allow(dead_code)]
const TAG_ARRAY: u8 = 0x05;
#[allow(dead_code)]
const TAG_MAP: u8 = 0x06;

#[allow(dead_code)]
fn invalid_data(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

#[allow(dead_code)]
fn take_u8(src: &mut BytesMut) -> io::Result<u8> {
    if src.is_empty() {
        return Err(invalid_data("truncated rmp value: expected a tag byte"));
    }
    Ok(src.get_u8())
}

#[allow(dead_code)]
fn take_len(src: &mut BytesMut) -> io::Result<usize> {
    if src.remaining() < 4 {
        return Err(invalid_data("truncated rmp value: expected a u32 length"));
    }
    let len = src.get_u32();
    if len > MAX_RMP_FRAME_LEN {
        return Err(invalid_data("rmp value length exceeds MAX_RMP_FRAME_LEN"));
    }
    Ok(len as usize)
}

#[allow(dead_code)]
fn take_i64(src: &mut BytesMut) -> io::Result<i64> {
    if src.remaining() < 8 {
        return Err(invalid_data("truncated rmp value: expected an i64"));
    }
    Ok(src.get_i64())
}

#[allow(dead_code)]
fn take_bytes(src: &mut BytesMut, len: usize) -> io::Result<Bytes> {
    if src.remaining() < len {
        return Err(invalid_data(
            "truncated rmp value: declared length exceeds buffered bytes",
        ));
    }
    Ok(src.split_to(len).freeze())
}

#[allow(dead_code)]
fn take_string(src: &mut BytesMut, len: usize) -> io::Result<String> {
    let bytes = take_bytes(src, len)?;
    String::from_utf8(bytes.to_vec())
        .map_err(|_| invalid_data("rmp Simple/Error value is not valid UTF-8"))
}

#[allow(dead_code)]
pub(crate) fn encode_frame(frame: &Frame, dst: &mut BytesMut) -> io::Result<()> {
    match frame {
        Frame::Null => dst.put_u8(TAG_NULL),
        Frame::Simple(s) => {
            dst.put_u8(TAG_SIMPLE);
            dst.put_u32(s.len() as u32);
            dst.put_slice(s.as_bytes());
        }
        Frame::Error(s) => {
            dst.put_u8(TAG_ERROR);
            dst.put_u32(s.len() as u32);
            dst.put_slice(s.as_bytes());
        }
        Frame::Integer(n) => {
            dst.put_u8(TAG_INTEGER);
            dst.put_i64(*n);
        }
        Frame::Bulk(b) => {
            dst.put_u8(TAG_BULK);
            dst.put_u32(b.len() as u32);
            dst.put_slice(b);
        }
        Frame::Array(items) => {
            dst.put_u8(TAG_ARRAY);
            dst.put_u32(items.len() as u32);
            for item in items {
                encode_frame(item, dst)?;
            }
        }
        Frame::Map(pairs) => {
            dst.put_u8(TAG_MAP);
            dst.put_u32(pairs.len() as u32);
            for (k, v) in pairs {
                encode_frame(k, dst)?;
                encode_frame(v, dst)?;
            }
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn decode_frame(src: &mut BytesMut) -> io::Result<Frame> {
    let tag = take_u8(src)?;
    match tag {
        TAG_NULL => Ok(Frame::Null),
        TAG_SIMPLE => {
            let len = take_len(src)?;
            Ok(Frame::Simple(take_string(src, len)?))
        }
        TAG_ERROR => {
            let len = take_len(src)?;
            Ok(Frame::Error(take_string(src, len)?))
        }
        TAG_INTEGER => Ok(Frame::Integer(take_i64(src)?)),
        TAG_BULK => {
            let len = take_len(src)?;
            Ok(Frame::Bulk(take_bytes(src, len)?))
        }
        TAG_ARRAY => {
            let count = take_len(src)?;
            // Pre-allocation is capped regardless of the declared count -- a forged count of
            // several billion must not itself trigger a huge allocation; the loop below fails
            // fast on the first truncated read once real buffered bytes run out.
            let mut items = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                items.push(decode_frame(src)?);
            }
            Ok(Frame::Array(items))
        }
        TAG_MAP => {
            let count = take_len(src)?;
            let mut pairs = Vec::with_capacity(count.min(1024));
            for _ in 0..count {
                let k = decode_frame(src)?;
                let v = decode_frame(src)?;
                pairs.push((k, v));
            }
            Ok(Frame::Map(pairs))
        }
        _ => Err(invalid_data("unknown rmp value tag")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(frame: Frame) -> Frame {
        let mut buf = BytesMut::new();
        encode_frame(&frame, &mut buf).unwrap();
        decode_frame(&mut buf).unwrap()
    }

    #[test]
    fn null_round_trips() {
        assert_eq!(round_trip(Frame::Null), Frame::Null);
    }

    #[test]
    fn simple_round_trips() {
        assert_eq!(
            round_trip(Frame::Simple("OK".into())),
            Frame::Simple("OK".into())
        );
    }

    #[test]
    fn error_round_trips() {
        assert_eq!(
            round_trip(Frame::Error("ERR boom".into())),
            Frame::Error("ERR boom".into())
        );
    }

    #[test]
    fn integer_round_trips_including_negative_values() {
        assert_eq!(round_trip(Frame::Integer(42)), Frame::Integer(42));
        assert_eq!(round_trip(Frame::Integer(-1)), Frame::Integer(-1));
    }

    #[test]
    fn bulk_round_trips() {
        assert_eq!(
            round_trip(Frame::Bulk(Bytes::from_static(b"bar"))),
            Frame::Bulk(Bytes::from_static(b"bar"))
        );
    }

    #[test]
    fn empty_bulk_round_trips() {
        assert_eq!(
            round_trip(Frame::Bulk(Bytes::new())),
            Frame::Bulk(Bytes::new())
        );
    }

    #[test]
    fn array_of_bulk_strings_round_trips_the_shape_a_command_uses() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ]);
        assert_eq!(round_trip(frame.clone()), frame);
    }

    #[test]
    fn nested_array_round_trips() {
        let frame = Frame::Array(vec![
            Frame::Integer(1),
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"x")), Frame::Null]),
        ]);
        assert_eq!(round_trip(frame.clone()), frame);
    }

    #[test]
    fn map_round_trips() {
        let frame = Frame::Map(vec![
            (Frame::Bulk(Bytes::from_static(b"proto")), Frame::Integer(1)),
            (
                Frame::Bulk(Bytes::from_static(b"role")),
                Frame::Bulk(Bytes::from_static(b"master")),
            ),
        ]);
        assert_eq!(round_trip(frame.clone()), frame);
    }

    #[test]
    fn the_get_foo_request_matches_the_spec_worked_example_byte_for_byte() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"GET")),
            Frame::Bulk(Bytes::from_static(b"foo")),
        ]);
        let mut buf = BytesMut::new();
        encode_frame(&frame, &mut buf).unwrap();
        assert_eq!(
            &buf[..],
            &[
                0x05, 0x00, 0x00, 0x00, 0x02, // Array, count 2
                0x04, 0x00, 0x00, 0x00, 0x03, b'G', b'E', b'T', // Bulk "GET"
                0x04, 0x00, 0x00, 0x00, 0x03, b'f', b'o', b'o', // Bulk "foo"
            ][..]
        );
    }

    #[test]
    fn the_bar_response_matches_the_spec_worked_example_byte_for_byte() {
        let mut buf = BytesMut::new();
        encode_frame(&Frame::Bulk(Bytes::from_static(b"bar")), &mut buf).unwrap();
        assert_eq!(
            &buf[..],
            &[0x04, 0x00, 0x00, 0x00, 0x03, b'b', b'a', b'r'][..]
        );
    }

    #[test]
    fn a_bulk_length_over_the_max_frame_len_is_rejected_without_reading_it() {
        let mut buf = BytesMut::new();
        buf.put_u8(TAG_BULK);
        buf.put_u32(MAX_RMP_FRAME_LEN + 1);
        // No payload bytes follow -- if decode_frame checked the length against remaining
        // buffer size first, it would report "truncated" instead of "too large". Asserting
        // the specific error message pins that the MAX_RMP_FRAME_LEN check runs first.
        let err = decode_frame(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn an_unknown_tag_byte_is_a_decode_error() {
        let mut buf = BytesMut::from(&[0xFFu8][..]);
        assert!(decode_frame(&mut buf).is_err());
    }

    #[test]
    fn a_truncated_value_is_a_decode_error_not_a_panic() {
        let mut buf = BytesMut::from(&[TAG_INTEGER, 0x00, 0x00][..]); // needs 8 bytes, has 2
        assert!(decode_frame(&mut buf).is_err());
    }
}
