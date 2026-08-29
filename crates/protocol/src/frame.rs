use bytes::Bytes;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
    Map(Vec<(Frame, Frame)>),
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

    #[test]
    fn map_frame_holds_key_value_pairs() {
        let f = Frame::Map(vec![(
            Frame::Bulk(Bytes::from_static(b"proto")),
            Frame::Integer(3),
        )]);
        assert_eq!(
            f,
            Frame::Map(vec![(
                Frame::Bulk(Bytes::from_static(b"proto")),
                Frame::Integer(3)
            )])
        );
    }

    #[test]
    fn map_frames_are_not_equal_to_array_frames_with_the_same_flattened_content() {
        let map = Frame::Map(vec![(Frame::Integer(1), Frame::Integer(2))]);
        let array = Frame::Array(vec![Frame::Integer(1), Frame::Integer(2)]);
        assert_ne!(map, array);
    }
}
