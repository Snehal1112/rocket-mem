//! Groups a decoded frame stream into transaction units, for the two places that apply frames
//! directly via `dispatcher::dispatch` outside the normal gated path: `aof::replay_with_stats`
//! and `replication::sync_once`. See
//! `docs/superpowers/specs/2026-09-10-multi-exec-transactions-spec.md`.

use protocol::Frame;

fn marker(frame: &Frame) -> Option<&'static str> {
    let Frame::Array(items) = frame else {
        return None;
    };
    let [Frame::Bulk(name_bytes)] = items.as_slice() else {
        return None; // MULTI/EXEC markers always have exactly zero arguments
    };
    match crate::dispatcher::upper_name(name_bytes)?.as_str() {
        "MULTI" => Some("MULTI"),
        "EXEC" => Some("EXEC"),
        _ => None,
    }
}

pub(crate) struct TransactionGrouper {
    buffer: Option<Vec<Frame>>,
}

impl TransactionGrouper {
    pub(crate) fn new() -> Self {
        Self { buffer: None }
    }

    pub(crate) fn is_mid_transaction(&self) -> bool {
        self.buffer.is_some()
    }

    pub(crate) fn feed(&mut self, frame: Frame) -> Result<Vec<Frame>, &'static str> {
        match marker(&frame) {
            Some("MULTI") => {
                if self.buffer.is_some() {
                    return Err("MULTI seen while already buffering a transaction");
                }
                self.buffer = Some(Vec::new());
                Ok(Vec::new())
            }
            Some("EXEC") => self.buffer.take().ok_or("EXEC seen with no matching MULTI"),
            _ => {
                if let Some(buffer) = self.buffer.as_mut() {
                    buffer.push(frame);
                    Ok(Vec::new())
                } else {
                    Ok(vec![frame])
                }
            }
        }
    }
}

impl Default for TransactionGrouper {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn bulk(s: &str) -> Frame {
        Frame::Bulk(Bytes::from(s.to_string()))
    }

    fn cmd(parts: &[&str]) -> Frame {
        Frame::Array(parts.iter().map(|p| bulk(p)).collect())
    }

    #[test]
    fn an_ordinary_frame_outside_any_transaction_passes_through_immediately() {
        let mut grouper = TransactionGrouper::new();
        let result = grouper.feed(cmd(&["SET", "k", "v"])).unwrap();
        assert_eq!(result, vec![cmd(&["SET", "k", "v"])]);
        assert!(!grouper.is_mid_transaction());
    }

    #[test]
    fn a_complete_transaction_yields_every_buffered_command_at_exec_in_order() {
        let mut grouper = TransactionGrouper::new();
        assert_eq!(grouper.feed(cmd(&["MULTI"])).unwrap(), Vec::<Frame>::new());
        assert!(grouper.is_mid_transaction());
        assert_eq!(
            grouper.feed(cmd(&["SET", "a", "1"])).unwrap(),
            Vec::<Frame>::new()
        );
        assert_eq!(
            grouper.feed(cmd(&["SET", "b", "2"])).unwrap(),
            Vec::<Frame>::new()
        );
        let result = grouper.feed(cmd(&["EXEC"])).unwrap();
        assert_eq!(
            result,
            vec![cmd(&["SET", "a", "1"]), cmd(&["SET", "b", "2"])]
        );
        assert!(!grouper.is_mid_transaction());
    }

    #[test]
    fn exec_with_no_matching_multi_is_an_error() {
        let mut grouper = TransactionGrouper::new();
        assert_eq!(
            grouper.feed(cmd(&["EXEC"])).unwrap_err(),
            "EXEC seen with no matching MULTI"
        );
    }

    #[test]
    fn a_nested_multi_is_an_error() {
        let mut grouper = TransactionGrouper::new();
        grouper.feed(cmd(&["MULTI"])).unwrap();
        assert_eq!(
            grouper.feed(cmd(&["MULTI"])).unwrap_err(),
            "MULTI seen while already buffering a transaction"
        );
    }
}
