use bytes::Bytes;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn frames_of_the_same_variant_and_content_are_equal() {
        assert_eq!(Frame::Simple("OK".into()), Frame::Simple("OK".into()));
        assert_eq!(
            Frame::Bulk(Bytes::from_static(b"x")),
            Frame::Bulk(Bytes::from_static(b"x"))
        );
    }

    #[test]
    fn frames_of_different_variants_are_not_equal() {
        assert_ne!(Frame::Simple("OK".into()), Frame::Error("OK".into()));
    }

    #[test]
    fn array_frame_holds_nested_frames() {
        let f = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"a")),
            Frame::Integer(1),
        ]);
        assert_eq!(
            f,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"a")),
                Frame::Integer(1)
            ])
        );
    }
}
