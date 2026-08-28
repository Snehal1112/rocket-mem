#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone)]
pub enum EngineError {
    #[error("WRONGTYPE Operation against a key holding the wrong kind of value")]
    WrongType,
    #[error("value is not an integer or out of range")]
    NotAnInteger,
}
