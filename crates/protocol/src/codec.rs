use crate::Frame;
use bytes::{BufMut, BytesMut};
use tokio_util::codec::Encoder;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Frame;
    use bytes::{Bytes, BytesMut};
    use tokio_util::codec::Encoder;

    #[test]
    fn encodes_simple_string() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Simple("OK".into()), &mut buf).unwrap();
        assert_eq!(&buf[..], b"+OK\r\n");
    }

    #[test]
    fn encodes_error() {
        let mut buf = BytesMut::new();
        RespCodec.encode(Frame::Error("ERR bad".into()), &mut buf).unwrap();
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
        RespCodec.encode(Frame::Bulk(Bytes::from_static(b"hi")), &mut buf).unwrap();
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
        let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"a")), Frame::Integer(1)]);
        RespCodec.encode(frame, &mut buf).unwrap();
        assert_eq!(&buf[..], b"*2\r\n$1\r\na\r\n:1\r\n");
    }
}
