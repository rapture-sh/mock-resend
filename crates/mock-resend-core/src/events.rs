//! The delivery event lifecycle.
//!
//! Only the event type itself is here so far. The transitions — which event may legally
//! follow which, and the timers that drive them — land with the webhook sender, because
//! an event nothing can observe is not worth modelling yet.

use std::fmt;

/// A delivery event, as Resend reports it in the `last_event` field and in the
/// `email.*` webhook names.
///
/// The string forms are Resend's own, so [`EmailEvent::as_str`] is the wire value in
/// both places: `last_event: "delivery_delayed"` and the `email.delivery_delayed`
/// webhook.
///
/// Note the spelling of [`EmailEvent::Canceled`]: Resend uses one `l`, and a mock that
/// "corrects" it breaks every caller matching on the string.
///
/// <https://resend.com/docs/dashboard/emails/introduction#understand-email-events>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EmailEvent {
    /// Queued for delivery. Resend uses this for broadcasts and batches.
    Queued,
    /// Held until `scheduled_at`.
    Scheduled,
    /// Accepted and handed off.
    Sent,
    /// Accepted by the recipient's mail server.
    Delivered,
    /// Deferred by a temporary problem at the recipient's mail server.
    DeliveryDelayed,
    /// Rejected by the recipient's mail server.
    Bounced,
    /// Delivered, then marked as spam by the recipient.
    Complained,
    /// Could not be sent.
    Failed,
    /// Opened by the recipient.
    Opened,
    /// A link in the message was clicked.
    Clicked,
    /// A scheduled send was cancelled before it went out.
    Canceled,
}

impl EmailEvent {
    /// The wire form, as it appears in `last_event`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Scheduled => "scheduled",
            Self::Sent => "sent",
            Self::Delivered => "delivered",
            Self::DeliveryDelayed => "delivery_delayed",
            Self::Bounced => "bounced",
            Self::Complained => "complained",
            Self::Failed => "failed",
            Self::Opened => "opened",
            Self::Clicked => "clicked",
            Self::Canceled => "canceled",
        }
    }

    /// The webhook event name Resend sends for this event.
    #[must_use]
    pub fn webhook_name(self) -> String {
        format!("email.{}", self.as_str())
    }
}

impl fmt::Display for EmailEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod email_event {
    use super::EmailEvent;

    #[test]
    fn should_spell_delivery_delayed_with_an_underscore() {
        assert_eq!(EmailEvent::DeliveryDelayed.as_str(), "delivery_delayed");
    }

    #[test]
    fn should_spell_canceled_the_way_resend_does() {
        assert_eq!(EmailEvent::Canceled.as_str(), "canceled");
    }

    #[test]
    fn should_prefix_webhook_names_with_email() {
        assert_eq!(EmailEvent::Bounced.webhook_name(), "email.bounced");
    }
}
