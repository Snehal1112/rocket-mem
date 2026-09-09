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

impl Frame {
    /// A static discriminant naming this frame's kind, safe to log at any level without ever
    /// rendering value contents -- unlike `{:?}` on `Frame`, which would dump a `Bulk`'s or an
    /// `Array`'s actual payload byte-for-byte.
    pub fn kind(&self) -> &'static str {
        match self {
            Frame::Simple(_) => "simple",
            Frame::Error(_) => "error",
            Frame::Integer(_) => "integer",
            Frame::Bulk(_) => "bulk",
            Frame::Null => "null",
            Frame::Array(_) => "array",
            Frame::Map(_) => "map",
        }
    }

    /// This frame's length for logging: byte length for `Simple`/`Error`/`Bulk`, element count
    /// for `Array`/`Map`, `0` for `Integer`/`Null` (neither has a natural length). Never the
    /// content itself.
    pub fn log_len(&self) -> usize {
        match self {
            Frame::Simple(s) => s.len(),
            Frame::Error(s) => s.len(),
            Frame::Integer(_) => 0,
            Frame::Bulk(b) => b.len(),
            Frame::Null => 0,
            Frame::Array(items) => items.len(),
            Frame::Map(pairs) => pairs.len(),
        }
    }
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

    #[test]
    fn kind_names_each_variant_without_touching_its_contents() {
        assert_eq!(Frame::Simple("OK".into()).kind(), "simple");
        assert_eq!(Frame::Error("ERR".into()).kind(), "error");
        assert_eq!(Frame::Integer(1).kind(), "integer");
        assert_eq!(Frame::Bulk(Bytes::from_static(b"x")).kind(), "bulk");
        assert_eq!(Frame::Null.kind(), "null");
        assert_eq!(Frame::Array(vec![]).kind(), "array");
        assert_eq!(Frame::Map(vec![]).kind(), "map");
    }

    #[test]
    fn log_len_reports_byte_length_for_string_shaped_frames() {
        assert_eq!(Frame::Simple("hello".into()).log_len(), 5);
        assert_eq!(Frame::Error("boom".into()).log_len(), 4);
        assert_eq!(Frame::Bulk(Bytes::from_static(b"abc")).log_len(), 3);
    }

    #[test]
    fn log_len_reports_element_count_for_container_frames() {
        assert_eq!(
            Frame::Array(vec![Frame::Integer(1), Frame::Integer(2)]).log_len(),
            2
        );
        assert_eq!(
            Frame::Map(vec![(Frame::Integer(1), Frame::Integer(2))]).log_len(),
            1
        );
    }

    #[test]
    fn log_len_is_zero_for_frames_with_no_natural_length() {
        assert_eq!(Frame::Integer(42).log_len(), 0);
        assert_eq!(Frame::Null.log_len(), 0);
    }
}
