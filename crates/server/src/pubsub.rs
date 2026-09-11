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
    patterns: Mutex<HashMap<Bytes, Vec<Subscriber>>>,
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

    /// Registers `client_id`'s outbound channel under `pattern`, returning how many
    /// subscribers `pattern` now has (including this one).
    #[allow(dead_code)]
    pub(crate) fn psubscribe(
        &self,
        pattern: Bytes,
        client_id: u64,
        tx: tokio::sync::mpsc::UnboundedSender<protocol::Frame>,
    ) -> usize {
        let mut patterns = self.patterns.lock().unwrap_or_else(|e| e.into_inner());
        let subs = patterns.entry(pattern).or_default();
        subs.push(Subscriber { client_id, tx });
        subs.len()
    }

    /// Removes `client_id`'s registration under `pattern`, if any, returning the remaining
    /// subscriber count for that pattern. Returns `0`, not an error, when `client_id` was never
    /// subscribed to `pattern` at all.
    #[allow(dead_code)]
    pub(crate) fn punsubscribe(&self, pattern: &[u8], client_id: u64) -> usize {
        let mut patterns = self.patterns.lock().unwrap_or_else(|e| e.into_inner());
        let Some(subs) = patterns.get_mut(pattern) else {
            return 0;
        };
        subs.retain(|s| s.client_id != client_id);
        subs.len()
    }

    /// Returns the list of all channels that have at least one subscriber.
    #[allow(dead_code)]
    pub(crate) fn channels(&self) -> Vec<Bytes> {
        self.channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    /// Returns the subscriber count for each requested channel.
    #[allow(dead_code)]
    pub(crate) fn num_sub(&self, channels: &[Bytes]) -> Vec<(Bytes, usize)> {
        let map = self.channels.lock().unwrap_or_else(|e| e.into_inner());
        channels
            .iter()
            .map(|c| (c.clone(), map.get(c.as_ref()).map_or(0, Vec::len)))
            .collect()
    }

    /// Returns the count of distinct registered patterns.
    #[allow(dead_code)]
    pub(crate) fn num_pat(&self) -> usize {
        self.patterns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Delivers `message` on `channel` to every current subscriber (exact match and pattern match),
    /// pruning any whose receiver has been dropped (its connection died). Returns how many sends
    /// succeeded. Never itself returns an error: one dead subscriber must not affect delivery to others.
    #[allow(dead_code)]
    pub(crate) fn publish(&self, channel: &[u8], message: &Bytes) -> usize {
        let mut delivered = 0;
        {
            let mut channels = self.channels.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(subs) = channels.get_mut(channel) {
                let frame = protocol::Frame::Push(vec![
                    protocol::Frame::Bulk(Bytes::from_static(b"message")),
                    protocol::Frame::Bulk(Bytes::copy_from_slice(channel)),
                    protocol::Frame::Bulk(message.clone()),
                ]);
                subs.retain(|s| {
                    let alive = s.tx.send(frame.clone()).is_ok();
                    if alive {
                        delivered += 1;
                    }
                    alive
                });
            }
        }
        {
            let mut patterns = self.patterns.lock().unwrap_or_else(|e| e.into_inner());
            patterns.retain(|pattern, subs| {
                if engine::glob::glob_match(pattern, channel) {
                    let frame = protocol::Frame::Push(vec![
                        protocol::Frame::Bulk(Bytes::from_static(b"pmessage")),
                        protocol::Frame::Bulk(pattern.clone()),
                        protocol::Frame::Bulk(Bytes::copy_from_slice(channel)),
                        protocol::Frame::Bulk(message.clone()),
                    ]);
                    subs.retain(|s| {
                        let alive = s.tx.send(frame.clone()).is_ok();
                        if alive {
                            delivered += 1;
                        }
                        alive
                    });
                }
                !subs.is_empty() || !engine::glob::glob_match(pattern, channel)
            });
        }
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

    #[test]
    fn publish_delivers_to_a_matching_pattern_as_a_pmessage_frame() {
        let registry = PubSubRegistry::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        registry.psubscribe(Bytes::from_static(b"news.*"), 1, tx);

        let delivered = registry.publish(b"news.sports", &Bytes::from_static(b"hello"));

        assert_eq!(delivered, 1);
        assert_eq!(
            rx.try_recv().unwrap(),
            protocol::Frame::Push(vec![
                protocol::Frame::Bulk(Bytes::from_static(b"pmessage")),
                protocol::Frame::Bulk(Bytes::from_static(b"news.*")),
                protocol::Frame::Bulk(Bytes::from_static(b"news.sports")),
                protocol::Frame::Bulk(Bytes::from_static(b"hello")),
            ])
        );
    }

    #[test]
    fn publish_delivers_to_both_exact_and_pattern_subscribers_of_the_same_channel() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news.sports"), 1, tx1);
        registry.psubscribe(Bytes::from_static(b"news.*"), 2, tx2);

        assert_eq!(
            registry.publish(b"news.sports", &Bytes::from_static(b"x")),
            2
        );
    }

    #[test]
    fn punsubscribe_returns_the_remaining_count_for_that_pattern() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.psubscribe(Bytes::from_static(b"news.*"), 1, tx1);
        registry.psubscribe(Bytes::from_static(b"news.*"), 2, tx2);

        assert_eq!(registry.punsubscribe(b"news.*", 1), 1);
    }

    #[test]
    fn channels_lists_every_channel_with_at_least_one_subscriber() {
        let registry = PubSubRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx);

        assert_eq!(registry.channels(), vec![Bytes::from_static(b"news")]);
    }

    #[test]
    fn num_sub_reports_the_subscriber_count_per_requested_channel() {
        let registry = PubSubRegistry::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        registry.subscribe(Bytes::from_static(b"news"), 1, tx);

        assert_eq!(
            registry.num_sub(&[Bytes::from_static(b"news"), Bytes::from_static(b"empty")]),
            vec![
                (Bytes::from_static(b"news"), 1),
                (Bytes::from_static(b"empty"), 0),
            ]
        );
    }

    #[test]
    fn num_pat_counts_distinct_registered_patterns() {
        let registry = PubSubRegistry::default();
        let (tx1, _rx1) = tokio::sync::mpsc::unbounded_channel();
        let (tx2, _rx2) = tokio::sync::mpsc::unbounded_channel();
        registry.psubscribe(Bytes::from_static(b"news.*"), 1, tx1);
        registry.psubscribe(Bytes::from_static(b"sports.*"), 2, tx2);

        assert_eq!(registry.num_pat(), 2);
    }
}
