use protocol::Frame;

#[derive(Debug)]
pub enum RmpError {
    Io(std::io::Error),
    ConnectionClosed,
    ServerError(String),
    UnexpectedReply(Frame),
}

impl std::fmt::Display for RmpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RmpError::Io(e) => write!(f, "rmp io error: {e}"),
            RmpError::ConnectionClosed => write!(f, "rmp connection closed"),
            RmpError::ServerError(msg) => write!(f, "{msg}"),
            RmpError::UnexpectedReply(frame) => write!(f, "unexpected rmp reply: {frame:?}"),
        }
    }
}

impl std::error::Error for RmpError {}

impl From<std::io::Error> for RmpError {
    fn from(e: std::io::Error) -> Self {
        RmpError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_error_displays_its_message() {
        let err = RmpError::ServerError("WRONGTYPE bad".to_string());
        assert_eq!(err.to_string(), "WRONGTYPE bad");
    }

    #[test]
    fn connection_closed_has_a_stable_message() {
        assert_eq!(
            RmpError::ConnectionClosed.to_string(),
            "rmp connection closed"
        );
    }
}
