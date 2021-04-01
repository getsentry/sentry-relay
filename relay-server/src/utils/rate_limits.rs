use std::{
    fmt::{self, Write},
    net::IpAddr,
    time::Instant,
};

use actix::Addr;
use relay_general::protocol::EventId;
use relay_quotas::{
    DataCategories, DataCategory, ItemScoping, QuotaScope, RateLimit, RateLimitScope, RateLimits,
    ReasonCode, Scoping,
};

use crate::{
    actors::outcome::{Outcome, OutcomeProducer, TrackOutcome},
    envelope::{Envelope, Item, ItemType},
};

/// Name of the rate limits header.
pub const RATE_LIMITS_HEADER: &str = "X-Sentry-Rate-Limits";

/// Formats the `X-Sentry-Rate-Limits` header.
pub fn format_rate_limits(rate_limits: &RateLimits) -> String {
    let mut header = String::new();

    for rate_limit in rate_limits {
        if !header.is_empty() {
            header.push_str(", ");
        }

        write!(header, "{}:", rate_limit.retry_after.remaining_seconds()).ok();

        for (index, category) in rate_limit.categories.iter().enumerate() {
            if index > 0 {
                header.push(';');
            }
            write!(header, "{}", category).ok();
        }

        write!(header, ":{}", rate_limit.scope.name()).ok();

        if let Some(ref reason_code) = rate_limit.reason_code {
            write!(header, ":{}", reason_code).ok();
        }
    }

    header
}

/// Parses the `X-Sentry-Rate-Limits` header.
pub fn parse_rate_limits(scoping: &Scoping, string: &str) -> RateLimits {
    let mut rate_limits = RateLimits::new();

    for limit in string.split(',') {
        let limit = limit.trim();
        if limit.is_empty() {
            continue;
        }

        let mut components = limit.split(':');

        let retry_after = match components.next().and_then(|s| s.parse().ok()) {
            Some(retry_after) => retry_after,
            None => continue,
        };

        let mut categories = DataCategories::new();
        for category in components.next().unwrap_or("").split(';') {
            if !category.is_empty() {
                categories.push(DataCategory::from_name(category));
            }
        }

        let quota_scope = QuotaScope::from_name(components.next().unwrap_or(""));
        let scope = RateLimitScope::for_quota(scoping, quota_scope);

        let reason_code = components.next().map(ReasonCode::new);

        rate_limits.add(RateLimit {
            categories,
            scope,
            reason_code,
            retry_after,
        });
    }

    rate_limits
}

/// Infer the data category from an item.
///
/// Categories depend mostly on the item type, with a few special cases:
/// - `Event`: the category is inferred from the event type. This requires the `event_type` header
///   to be set on the event item.
/// - `Attachment`: If the attachment creates an event (e.g. for minidumps), the category is assumed
///   to be `Error`.
fn infer_event_category(item: &Item) -> Option<DataCategory> {
    match item.ty() {
        ItemType::Event => Some(DataCategory::Error),
        ItemType::Transaction => Some(DataCategory::Transaction),
        ItemType::Security | ItemType::RawSecurity => Some(DataCategory::Security),
        ItemType::UnrealReport => Some(DataCategory::Error),
        ItemType::Attachment if item.creates_event() => Some(DataCategory::Error),
        ItemType::Attachment => None,
        ItemType::Session => None,
        ItemType::Sessions => None,
        ItemType::Metrics => None,
        ItemType::MetricBuckets => None,
        ItemType::FormData => None,
        ItemType::UserReport => None,
    }
}

/// A summary of `Envelope` contents.
///
/// Summarizes the contained event, size of attachments, session updates, and whether there are
/// plain attachments. This is used for efficient rate limiting or outcome handling.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvelopeSummary {
    /// The data category of the event in the envelope. `None` if there is no event.
    pub event_category: Option<DataCategory>,

    /// The quantity of all attachments combined in bytes.
    pub attachment_quantity: usize,

    /// The number of all session updates.
    pub session_quantity: usize,

    /// Indicates that the envelope contains regular attachments that do not create event payloads.
    pub has_plain_attachments: bool,

    /// Unique identifier of the event associated to this envelope.
    pub event_id: Option<EventId>,

    /// The IP address of the client that the envelope's event originates from.
    pub remote_addr: Option<IpAddr>,
}

impl EnvelopeSummary {
    /// Creates an empty summary.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Creates an envelope summary and aggregates the given envelope.
    pub fn compute(envelope: &Envelope) -> Self {
        let mut summary = Self::empty();

        summary.event_id = envelope.event_id();
        summary.remote_addr = envelope.meta().client_addr();

        for item in envelope.items() {
            if item.creates_event() {
                summary.infer_category(item);
            } else if item.ty() == ItemType::Attachment {
                // Plain attachments do not create events.
                summary.has_plain_attachments = true;
            }

            // If the item has been rate limited before, the quota has been consumed and outcomes
            // emitted. We can skip it here.
            if item.rate_limited() {
                continue;
            }

            match item.ty() {
                ItemType::Attachment => summary.attachment_quantity += item.len().max(1),
                ItemType::Session => summary.session_quantity += 1,
                _ => (),
            }
        }

        summary
    }

    fn infer_category(&mut self, item: &Item) {
        if matches!(self.event_category, None | Some(DataCategory::Default)) {
            if let Some(category) = infer_event_category(item) {
                self.event_category = Some(category);
            }
        }
    }
}

struct ItemRetention {
    applied_limit: Option<RateLimit>,
    retain_in_envelope: bool,
}

struct RateLimitForItem {
    applied_limit: RateLimit,
    item_category: DataCategory,
}

/// Enforces rate limits with the given `check` function on items in the envelope.
///
/// The `check` function is called with the following rules:
///  - Once for a single event, if present in the envelope.
///  - Once for all comprised attachments, unless the event was rate limited.
///  - Once for all comprised sessions.
///
/// Items violating the rate limit are removed from the envelope. This follows a set of rules:
///  - If the event is removed, all items depending on the event are removed (e.g. attachments).
///  - Attachments are not removed if they create events (e.g. minidumps).
///  - Sessions are handled separate to all of the above.
pub struct EnvelopeLimiter<F> {
    check: F,
    event_category: Option<DataCategory>,
    event_limit: Option<RateLimit>,
    attachment_limit: Option<RateLimit>,
    session_limit: Option<RateLimit>,
}

impl<E, F> EnvelopeLimiter<F>
where
    F: FnMut(ItemScoping<'_>, usize) -> Result<RateLimits, E>,
{
    /// Create a new `EnvelopeLimiter` with the given `check` function.
    pub fn new(check: F) -> Self {
        Self {
            check,
            event_category: None,
            event_limit: None,
            attachment_limit: None,
            session_limit: None,
        }
    }

    /// Assume an event with the given category, even if no item is present in the envelope.
    ///
    /// This ensures that rate limits for the given data category are checked even if there is no
    /// matching item in the envelope. Other items are handled according to the rules as if the
    /// event item were present.
    #[cfg(feature = "processing")]
    pub fn assume_event(&mut self, category: DataCategory) {
        self.event_category = Some(category);
    }

    /// Process rate limits for the envelope, removing offending items and returning applied limits.
    pub fn enforce(
        mut self,
        envelope: &mut Envelope,
        scoping: &Scoping,
    ) -> Result<RateLimitEnforcement, E> {
        let mut summary = EnvelopeSummary::compute(envelope);
        if let Some(event_category) = self.event_category {
            summary.event_category = Some(event_category);
        }

        let applied_limits = self.execute(&summary, scoping)?;
        let limited_items = self.apply_retention(envelope);
        Ok(RateLimitEnforcement {
            summary,
            applied_limits,
            limited_items,
        })
    }

    fn apply_retention(&mut self, envelope: &mut Envelope) -> Vec<RateLimitForItem> {
        let mut applied_limits = vec![];
        envelope.retain_items(|item| {
            let retention = self.retain_item(item);
            if let Some(applied_limit) = retention.applied_limit {
                if let Some(item_category) = infer_event_category(item) {
                    applied_limits.push(RateLimitForItem {
                        applied_limit,
                        item_category,
                    })
                }
            }
            retention.retain_in_envelope
        });

        applied_limits
    }

    fn execute(&mut self, summary: &EnvelopeSummary, scoping: &Scoping) -> Result<RateLimits, E> {
        let mut rate_limits = RateLimits::new();

        if let Some(category) = summary.event_category {
            let event_limits = (&mut self.check)(scoping.item(category), 1)?;
            self.event_limit = event_limits.get_active_limit().map(RateLimit::clone);
            rate_limits.merge(event_limits);
        }

        if self.event_limit.is_none() && summary.attachment_quantity > 0 {
            let item_scoping = scoping.item(DataCategory::Attachment);
            let attachment_limits = (&mut self.check)(item_scoping, summary.attachment_quantity)?;
            self.attachment_limit = attachment_limits.get_active_limit().map(RateLimit::clone);

            // Only record rate limits for plain attachments. For all other attachments, it's
            // perfectly "legal" to send them. They will still be discarded in Sentry, but clients
            // can continue to send them.
            if summary.has_plain_attachments {
                rate_limits.merge(attachment_limits);
            }
        }

        if summary.session_quantity > 0 {
            let item_scoping = scoping.item(DataCategory::Session);
            let session_limits = (&mut self.check)(item_scoping, summary.session_quantity)?;
            self.session_limit = session_limits.get_active_limit().map(RateLimit::clone);
            rate_limits.merge(session_limits);
        }

        Ok(rate_limits)
    }

    fn retain_item(&self, item: &mut Item) -> ItemRetention {
        // Remove event items and all items that depend on this event
        if let Some(event_limit) = &self.event_limit {
            if item.requires_event() {
                return ItemRetention {
                    applied_limit: Some(event_limit.clone()),
                    retain_in_envelope: false,
                };
            }
        }

        // Remove attachments, except those required for processing
        if let Some(attachment_limit) = &self.attachment_limit {
            if item.ty() == ItemType::Attachment {
                if item.creates_event() {
                    let applied_limit = if item.rate_limited() {
                        None
                    } else {
                        item.set_rate_limited(true);
                        Some(attachment_limit.clone())
                    };
                    return ItemRetention {
                        applied_limit,
                        retain_in_envelope: true,
                    };
                } else {
                    return ItemRetention {
                        applied_limit: Some(attachment_limit.clone()),
                        retain_in_envelope: false,
                    };
                }
            }
        }

        // Remove sessions independently of events
        if let Some(session_limit) = &self.session_limit {
            if item.ty() == ItemType::Session {
                return ItemRetention {
                    applied_limit: Some(session_limit.clone()),
                    retain_in_envelope: false,
                };
            }
        }

        ItemRetention {
            applied_limit: None,
            retain_in_envelope: true,
        }
    }
}

impl<F> fmt::Debug for EnvelopeLimiter<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvelopeLimiter")
            .field("event_category", &self.event_category)
            .field("event_limit", &self.event_limit)
            .field("attachment_limit", &self.attachment_limit)
            .field("session_limit", &self.session_limit)
            .finish()
    }
}

pub struct RateLimitEnforcement {
    pub applied_limits: RateLimits,
    summary: EnvelopeSummary,
    limited_items: Vec<RateLimitForItem>,
}

impl RateLimitEnforcement {
    pub fn emit_outcomes(&self, scoping: &Scoping, outcome_producer: &Addr<OutcomeProducer>) {
        let timestamp = Instant::now();
        for limited_item in self.limited_items.iter() {
            let reason_code = &limited_item.applied_limit.reason_code;
            let category = limited_item.item_category;
            outcome_producer.do_send(TrackOutcome {
                timestamp,
                scoping: *scoping,
                outcome: Outcome::RateLimited(reason_code.clone()),
                event_id: self.summary.event_id,
                remote_addr: self.summary.remote_addr,
                category,
                quantity: match category {
                    DataCategory::Attachment => self.summary.attachment_quantity,
                    _ => 1,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use smallvec::smallvec;

    use relay_common::{ProjectId, ProjectKey};
    use relay_quotas::RetryAfter;

    use crate::envelope::{AttachmentType, ContentType};

    #[test]
    fn test_format_rate_limits() {
        let mut rate_limits = RateLimits::new();

        // Add a generic rate limit for all categories.
        rate_limits.add(RateLimit {
            categories: DataCategories::new(),
            scope: RateLimitScope::Organization(42),
            reason_code: Some(ReasonCode::new("my_limit")),
            retry_after: RetryAfter::from_secs(42),
        });

        // Add a more specific rate limit for just one category.
        rate_limits.add(RateLimit {
            categories: smallvec![DataCategory::Transaction, DataCategory::Security],
            scope: RateLimitScope::Project(ProjectId::new(21)),
            reason_code: None,
            retry_after: RetryAfter::from_secs(4711),
        });

        let formatted = format_rate_limits(&rate_limits);
        let expected = "42::organization:my_limit, 4711:transaction;security:project";
        assert_eq!(formatted, expected);
    }

    #[test]
    fn test_parse_invalid_rate_limits() {
        let scoping = Scoping {
            organization_id: 42,
            project_id: ProjectId::new(21),
            public_key: ProjectKey::parse("a94ae32be2584e0bbd7a4cbb95971fee").unwrap(),
            key_id: Some(17),
        };

        assert!(parse_rate_limits(&scoping, "").is_ok());
        assert!(parse_rate_limits(&scoping, "invalid").is_ok());
        assert!(parse_rate_limits(&scoping, ",,,").is_ok());
    }

    #[test]
    fn test_parse_rate_limits() {
        let scoping = Scoping {
            organization_id: 42,
            project_id: ProjectId::new(21),
            public_key: ProjectKey::parse("a94ae32be2584e0bbd7a4cbb95971fee").unwrap(),
            key_id: Some(17),
        };

        // contains "foobar", an unknown scope that should be mapped to Unknown
        let formatted =
            "42::organization:my_limit, invalid, 4711:foobar;transaction;security:project";
        let rate_limits: Vec<RateLimit> =
            parse_rate_limits(&scoping, formatted).into_iter().collect();

        assert_eq!(
            rate_limits,
            vec![
                RateLimit {
                    categories: DataCategories::new(),
                    scope: RateLimitScope::Organization(42),
                    reason_code: Some(ReasonCode::new("my_limit")),
                    retry_after: rate_limits[0].retry_after,
                },
                RateLimit {
                    categories: smallvec![
                        DataCategory::Unknown,
                        DataCategory::Transaction,
                        DataCategory::Security,
                    ],
                    scope: RateLimitScope::Project(ProjectId::new(21)),
                    reason_code: None,
                    retry_after: rate_limits[1].retry_after,
                }
            ]
        );

        assert_eq!(42, rate_limits[0].retry_after.remaining_seconds());
        assert_eq!(4711, rate_limits[1].retry_after.remaining_seconds());
    }

    #[test]
    fn test_parse_rate_limits_only_unknown() {
        let scoping = Scoping {
            organization_id: 42,
            project_id: ProjectId::new(21),
            public_key: ProjectKey::parse("a94ae32be2584e0bbd7a4cbb95971fee").unwrap(),
            key_id: Some(17),
        };

        // contains "foobar", an unknown scope that should be mapped to Unknown
        let formatted = "42:foo;bar:organization";
        let rate_limits: Vec<RateLimit> =
            parse_rate_limits(&scoping, formatted).into_iter().collect();

        assert_eq!(
            rate_limits,
            vec![RateLimit {
                categories: smallvec![DataCategory::Unknown, DataCategory::Unknown],
                scope: RateLimitScope::Organization(42),
                reason_code: None,
                retry_after: rate_limits[0].retry_after,
            },]
        );
    }

    macro_rules! envelope {
        ($( $item_type:ident $( :: $attachment_type:ident )? ),*) => {{
            let bytes = "{\"dsn\":\"https://e12d836b15bb49d7bbf99e64295d995b:@sentry.io/42\"}";
            #[allow(unused_mut)]
            let mut envelope = Envelope::parse_bytes(bytes.into()).unwrap();
            $(
                let mut item = Item::new(ItemType::$item_type);
                item.set_payload(ContentType::OctetStream, "0123456789");
                $( item.set_attachment_type(AttachmentType::$attachment_type); )?
                envelope.add_item(item);
            )*
            envelope
        }}
    }

    fn scoping() -> Scoping {
        Scoping {
            organization_id: 42,
            project_id: ProjectId::new(21),
            public_key: ProjectKey::parse("e12d836b15bb49d7bbf99e64295d995b").unwrap(),
            key_id: Some(17),
        }
    }

    fn rate_limit(category: DataCategory) -> RateLimit {
        RateLimit {
            categories: vec![category].into(),
            scope: RateLimitScope::Organization(42),
            reason_code: None,
            retry_after: RetryAfter::from_secs(60),
        }
    }

    #[derive(Debug, Default)]
    struct MockLimiter {
        denied: Vec<DataCategory>,
        called: BTreeMap<DataCategory, usize>,
    }

    impl MockLimiter {
        pub fn deny(mut self, category: DataCategory) -> Self {
            self.denied.push(category);
            self
        }

        pub fn check(
            &mut self,
            scoping: ItemScoping<'_>,
            quantity: usize,
        ) -> Result<RateLimits, ()> {
            let cat = scoping.category;
            let previous = self.called.insert(cat, quantity);
            assert!(previous.is_none(), "rate limiter invoked twice for {}", cat);

            let mut limits = RateLimits::new();
            if self.denied.contains(&cat) {
                limits.add(rate_limit(cat));
            }
            Ok(limits)
        }

        pub fn assert_call(&self, category: DataCategory, quantity: Option<usize>) {
            assert_eq!(self.called.get(&category), quantity.as_ref());
        }
    }

    #[test]
    fn test_enforce_pass_empty() {
        let mut envelope = envelope![];

        let mut mock = MockLimiter::default();
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        assert!(!limits.is_limited());
        assert!(envelope.is_empty());
        mock.assert_call(DataCategory::Error, None);
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    fn test_enforce_limit_error_event() {
        let mut envelope = envelope![Event];

        let mut mock = MockLimiter::default().deny(DataCategory::Error);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        assert!(limits.is_limited());
        assert!(envelope.is_empty());
        mock.assert_call(DataCategory::Error, Some(1));
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    fn test_enforce_limit_error_with_attachments() {
        let mut envelope = envelope![Event, Attachment];

        let mut mock = MockLimiter::default().deny(DataCategory::Error);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        assert!(limits.is_limited());
        assert!(envelope.is_empty());
        mock.assert_call(DataCategory::Error, Some(1));
        // Error is limited, so no need to call the attachment quota
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    fn test_enforce_limit_minidump() {
        let mut envelope = envelope![Attachment::Minidump];

        let mut mock = MockLimiter::default().deny(DataCategory::Error);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        assert!(limits.is_limited());
        assert!(envelope.is_empty());
        mock.assert_call(DataCategory::Error, Some(1));
        // Error is limited, so no need to call the attachment quota
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    fn test_enforce_limit_attachments() {
        let mut envelope = envelope![Attachment::Minidump, Attachment];

        let mut mock = MockLimiter::default().deny(DataCategory::Attachment);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        // Attachments would be limited, but crash reports create events and are thus allowed.
        assert!(limits.is_limited());
        assert_eq!(envelope.len(), 1);
        mock.assert_call(DataCategory::Error, Some(1));
        mock.assert_call(DataCategory::Attachment, Some(20));
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    fn test_enforce_pass_minidump() {
        let mut envelope = envelope![Attachment::Minidump];

        let mut mock = MockLimiter::default().deny(DataCategory::Attachment);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        // If only crash report attachments are present, we don't emit a rate limit.
        assert!(!limits.is_limited());
        assert_eq!(envelope.len(), 1);
        mock.assert_call(DataCategory::Error, Some(1));
        mock.assert_call(DataCategory::Attachment, Some(10));
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    fn test_enforce_skip_rate_limited() {
        let mut envelope = envelope![];

        let mut item = Item::new(ItemType::Attachment);
        item.set_payload(ContentType::OctetStream, "0123456789");
        item.set_rate_limited(true);
        envelope.add_item(item);

        let mut mock = MockLimiter::default().deny(DataCategory::Error);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        assert!(!limits.is_limited()); // No new rate limits applied.
        assert_eq!(envelope.len(), 1); // The item was retained
        mock.assert_call(DataCategory::Error, None);
        mock.assert_call(DataCategory::Attachment, None); // Limiter not invoked
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    fn test_enforce_pass_sessions() {
        let mut envelope = envelope![Session, Session, Session];

        let mut mock = MockLimiter::default().deny(DataCategory::Error);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        // If only crash report attachments are present, we don't emit a rate limit.
        assert!(!limits.is_limited());
        assert_eq!(envelope.len(), 3);
        mock.assert_call(DataCategory::Error, None);
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, Some(3));
    }

    #[test]
    fn test_enforce_limit_sessions() {
        let mut envelope = envelope![Session, Session, Event];

        let mut mock = MockLimiter::default().deny(DataCategory::Session);
        let enforcement = EnvelopeLimiter::new(|s, q| mock.check(s, q))
            .enforce(&mut envelope, &scoping())
            .unwrap();
        let limits = enforcement.applied_limits;

        // If only crash report attachments are present, we don't emit a rate limit.
        assert!(limits.is_limited());
        assert_eq!(envelope.len(), 1);
        mock.assert_call(DataCategory::Error, Some(1));
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, Some(2));
    }

    #[test]
    #[cfg(feature = "processing")]
    fn test_enforce_limit_assumed_event() {
        let mut envelope = envelope![];

        let mut mock = MockLimiter::default().deny(DataCategory::Transaction);
        let mut limiter = EnvelopeLimiter::new(|s, q| mock.check(s, q));
        limiter.assume_event(DataCategory::Transaction);
        let enforcement = limiter.enforce(&mut envelope, &scoping()).unwrap();
        let limits = enforcement.rate_limits;

        assert!(limits.is_limited());
        assert!(envelope.is_empty()); // obviously
        mock.assert_call(DataCategory::Transaction, Some(1));
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, None);
    }

    #[test]
    #[cfg(feature = "processing")]
    fn test_enforce_limit_assumed_attachments() {
        let mut envelope = envelope![Attachment, Attachment];

        let mut mock = MockLimiter::default().deny(DataCategory::Error);
        let mut limiter = EnvelopeLimiter::new(|s, q| mock.check(s, q));
        limiter.assume_event(DataCategory::Error);
        let enforcement = limiter.enforce(&mut envelope, &scoping()).unwrap();
        let limits = enforcement.rate_limits;

        assert!(limits.is_limited());
        assert!(envelope.is_empty());
        mock.assert_call(DataCategory::Error, Some(1));
        mock.assert_call(DataCategory::Attachment, None);
        mock.assert_call(DataCategory::Session, None);
    }
}
