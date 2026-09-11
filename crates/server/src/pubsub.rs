use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Mutex;

/// One connection's registration under a channel or pattern: which connection (for
/// `PUBSUB NUMSUB`-style bookkeeping and `remove_all`'s disconnect cleanup) and the outbound
/// channel `PubSubRegistry::publish` pushes messages through.
#[allow(dead_code)]
struct Subscriber {
    client_id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>,
}

/// Maps channel/pattern names to their live subscribers. Structurally the same
/// "register a sender, broadcast prunes dead ones" shape `replication::ReplicaRegistry` uses,
/// keyed by channel/pattern instead of by replica address. Both maps use `std::sync::Mutex`, not
/// `tokio::sync::Mutex`: every access here is a quick, synchronous map operation, never held
/// across an `.await` -- matching `ReplicaRegistry`'s own justification for the same choice.
#[derive(Default)]
#[allow(dead_code)]
pub(crate) struct PubSubRegistry {
    channels: Mutex<HashMap<Bytes, Vec<Subscriber>>>,
}

impl PubSubRegistry {
    /// Registers `client_id`'s outbound channel under `channel`, returning how many
    /// subscribers `channel` now has (including this one) -- the count `SUBSCRIBE`'s reply
    /// needs.
    #[allow(dead_code)]
    pub(crate) fn subscribe(
        &self,
        channel: Bytes,
        client_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>,
    ) -> usize {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let subs = channels.entry(channel).or_default();
        subs.push(Subscriber { client_id, tx });
        subs.len()
    }

    /// Removes `client_id`'s registration under `channel`, if any, returning the remaining
    /// subscriber count for that channel. Returns `0`, not an error, when `client_id` was never
    /// subscribed to `channel` at all -- matching real Redis, which never errors on an
    /// `UNSUBSCRIBE` of a channel the client never subscribed to.
    #[allow(dead_code)]
    pub(crate) fn unsubscribe(&self, channel: &[u8], client_id: u64) -> usize {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let Some(subs) = channels.get_mut(channel) else {
            return 0;
        };
        subs.retain(|s| s.client_id != client_id);
        subs.len()
    }

    /// Delivers `message` on `channel` to every current subscriber, pruning any whose receiver
    /// has been dropped (its connection died). Returns how many sends succeeded. Never itself
    /// returns an error: one dead subscriber must not affect delivery to the others.
    #[allow(dead_code)]
    pub(crate) fn publish(&self, channel: &[u8], message: &Bytes) -> usize {
        let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        let Some(subs) = channels.get_mut(channel) else {
            return 0;
        };
        let frame = protocol::Frame::Push(vec![
            protocol::Frame::Bulk(Bytes::from_static(b"message")),
            protocol::Frame::Bulk(Bytes::copy_from_slice(channel)),
            protocol::Frame::Bulk(message.clone()),
        ]);
        let mut delivered = 0;
        subs.retain(|s| {
            let alive = s.tx.send(frame.clone()).is_ok();
            if alive {
                delivered += 1;
            }
            alive
        });
        delivered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn subscribe_returns_the_subscriber_count_for_that_channel() {
        let registry = PubSubRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let count = registry.subscribe(Bytes::from_static(b"news"), 1, tx);
        assert_eq!(count, 1);
    }

    #[test]
    fn publish_delivers_to_every_subscriber_of_that_exact_channel() {
        let registry = PubSubRegistry::default();
        let (tx1, mut rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx1);
        registry.subscribe(Bytes::from_static(b"news"), 2, tx2);

        let delivered = registry.publish(b"news", &Bytes::from_static(b"hello"));

        assert_eq!(delivered, 2);
        assert_eq!(
            rx1.try_recv().unwrap(),
            protocol::Frame::Push(vec![
                protocol::Frame::Bulk(Bytes::from_static(b"message")),
                protocol::Frame::Bulk(Bytes::from_static(b"news")),
                protocol::Frame::Bulk(Bytes::from_static(b"hello")),
            ])
        );
        assert_eq!(
            rx2.try_recv().unwrap(),
            protocol::Frame::Push(vec![
                protocol::Frame::Bulk(Bytes::from_static(b"message")),
                protocol::Frame::Bulk(Bytes::from_static(b"news")),
                protocol::Frame::Bulk(Bytes::from_static(b"hello")),
            ])
        );
    }

    #[test]
    fn publish_to_a_channel_with_no_subscribers_delivers_to_nobody() {
        let registry = PubSubRegistry::default();
        assert_eq!(
            registry.publish(b"nobody-listening", &Bytes::from_static(b"x")),
            0
        );
    }

    #[test]
    fn publish_prunes_a_subscriber_whose_receiver_was_dropped() {
        let registry = PubSubRegistry::default();
        let (tx1, rx1) = tokio::sync::mpsc::unbounded_channel();
        drop(rx1); // receiver gone -- this subscriber is dead
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx1);
        registry.subscribe(Bytes::from_static(b"news"), 2, tx2);

        let delivered = registry.publish(b"news", &Bytes::from_static(b"hello"));

        assert_eq!(delivered, 1); // only the live subscriber counted
        assert!(rx2.try_recv().is_ok());

        // The dead subscriber is gone from the registry, not just skipped this once.
        assert_eq!(registry.publish(b"news", &Bytes::from_static(b"again")), 1);
    }

    #[test]
    fn unsubscribe_returns_the_remaining_count_for_that_channel() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx1);
        registry.subscribe(Bytes::from_static(b"news"), 2, tx2);

        let remaining = registry.unsubscribe(b"news", 1);

        assert_eq!(remaining, 1);
    }

    #[test]
    fn unsubscribe_from_a_channel_never_subscribed_to_returns_zero_without_panicking() {
        let registry = PubSubRegistry::default();
        assert_eq!(registry.unsubscribe(b"never-subscribed", 1), 0);
    }
}
