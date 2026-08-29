#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum EngineError {
    #[error("WRONGTYPE Operation against a key holding the wrong kind of value")]
    WrongType,
    #[error("value is not an integer or out of range")]
    NotAnInteger,
    #[error("no such key")]
    NoSuchKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_such_key_has_the_expected_display_text() {
        assert_eq!(EngineError::NoSuchKey.to_string(), "no such key");
    }
}
