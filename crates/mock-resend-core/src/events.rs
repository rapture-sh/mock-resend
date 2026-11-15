//! The delivery event lifecycle and which transitions are legal.
//!
//! An event is never appended from a handler. The engine decides whether `delivered` can
//! follow `bounced`, which is the whole reason this table is in one place: a mock that
//! lets a caller drive a message into an impossible state is a mock that makes broken
//! event handling look correct.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EmailEvent {
    /// Queued for delivery.
    ///
    /// Resend reports this for broadcasts and batches. mock-resend does **not**: a
    /// batched message goes straight to [`EmailEvent::Sent`], because the alternative
    /// is an intermediate state that real Resend passes through in milliseconds and a
    /// test would have to race. Listed in the README as a divergence.
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

/// Every event, for exhaustive iteration in tests and in the admin API.
pub const ALL_EVENTS: [EmailEvent; 11] = [
    EmailEvent::Queued,
    EmailEvent::Scheduled,
    EmailEvent::Sent,
    EmailEvent::Delivered,
    EmailEvent::DeliveryDelayed,
    EmailEvent::Bounced,
    EmailEvent::Complained,
    EmailEvent::Failed,
    EmailEvent::Opened,
    EmailEvent::Clicked,
    EmailEvent::Canceled,
];

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

    /// Parses the wire form.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        ALL_EVENTS.into_iter().find(|event| event.as_str() == value)
    }

    /// The webhook event name Resend sends for this event, where there is one.
    ///
    /// [`EmailEvent::Canceled`] and [`EmailEvent::Queued`] return `None`: they are
    /// `last_event` values with no corresponding `email.*` webhook in Resend's event
    /// type list, so cancelling a scheduled send changes the retrieved state without
    /// notifying a handler.
    ///
    /// <https://resend.com/docs/dashboard/webhooks/event-types>
    #[must_use]
    pub const fn webhook_name(self) -> Option<&'static str> {
        match self {
            Self::Scheduled => Some("email.scheduled"),
            Self::Sent => Some("email.sent"),
            Self::Delivered => Some("email.delivered"),
            Self::DeliveryDelayed => Some("email.delivery_delayed"),
            Self::Bounced => Some("email.bounced"),
            Self::Complained => Some("email.complained"),
            Self::Failed => Some("email.failed"),
            Self::Opened => Some("email.opened"),
            Self::Clicked => Some("email.clicked"),
            Self::Queued | Self::Canceled => None,
        }
    }

    /// Whether nothing can follow this event.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Bounced | Self::Complained | Self::Failed | Self::Canceled
        )
    }

    /// Whether a message at this event is still waiting for its send time.
    #[must_use]
    pub const fn is_scheduled(self) -> bool {
        matches!(self, Self::Scheduled)
    }

    /// Whether `next` may legally follow `self`.
    ///
    /// The shape of the table: a message moves forward through acceptance, hand-off and
    /// delivery, picks up engagement events after delivery, and stops at a terminal
    /// event. It never moves backwards, and it never leaves a terminal state.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() {
            return false;
        }

        match self {
            Self::Queued => matches!(next, Self::Scheduled | Self::Sent | Self::Failed),
            Self::Scheduled => matches!(next, Self::Sent | Self::Canceled | Self::Failed),
            Self::Sent => matches!(
                next,
                Self::Delivered
                    | Self::DeliveryDelayed
                    | Self::Bounced
                    | Self::Complained
                    | Self::Failed
                    | Self::Opened
                    | Self::Clicked
            ),
            // A repeated delay is normal: a receiving server can defer more than once
            // before it either accepts or gives up.
            Self::DeliveryDelayed => matches!(
                next,
                Self::Delivered | Self::DeliveryDelayed | Self::Bounced | Self::Failed
            ),
            // Engagement only, and a complaint. A delivered message cannot bounce.
            Self::Delivered => matches!(next, Self::Opened | Self::Clicked | Self::Complained),
            // Opens and clicks repeat, and either can follow the other: `last_event`
            // reports the most recent one, not a sequence.
            Self::Opened | Self::Clicked => {
                matches!(next, Self::Opened | Self::Clicked | Self::Complained)
            }
            Self::Bounced | Self::Complained | Self::Failed | Self::Canceled => false,
        }
    }
}

impl fmt::Display for EmailEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod email_event {
    use super::{ALL_EVENTS, EmailEvent};

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
        assert_eq!(EmailEvent::Bounced.webhook_name(), Some("email.bounced"));
    }

    #[test]
    fn should_have_no_webhook_name_for_a_cancellation() {
        // Resend's event type list has no `email.canceled`, so a cancelled send changes
        // `last_event` without notifying any handler.
        assert_eq!(EmailEvent::Canceled.webhook_name(), None);
        assert_eq!(EmailEvent::Queued.webhook_name(), None);
    }

    #[test]
    fn should_round_trip_every_event_through_its_wire_form() {
        for event in ALL_EVENTS {
            assert_eq!(EmailEvent::parse(event.as_str()), Some(event));
        }
    }

    #[test]
    fn should_not_parse_an_unknown_event() {
        assert_eq!(EmailEvent::parse("email.sent"), None);
        assert_eq!(EmailEvent::parse("cancelled"), None);
    }

    #[test]
    fn should_not_let_delivered_follow_bounced() {
        assert!(!EmailEvent::Bounced.can_transition_to(EmailEvent::Delivered));
    }

    #[test]
    fn should_not_let_anything_follow_a_terminal_event() {
        for terminal in ALL_EVENTS.into_iter().filter(|event| event.is_terminal()) {
            for next in ALL_EVENTS {
                assert!(
                    !terminal.can_transition_to(next),
                    "{terminal} should be terminal, but {next} was allowed to follow it"
                );
            }
        }
    }

    #[test]
    fn should_treat_bounced_complained_failed_and_canceled_as_terminal() {
        let terminal: Vec<EmailEvent> = ALL_EVENTS
            .into_iter()
            .filter(|event| event.is_terminal())
            .collect();

        assert_eq!(
            terminal,
            [
                EmailEvent::Bounced,
                EmailEvent::Complained,
                EmailEvent::Failed,
                EmailEvent::Canceled
            ]
        );
    }

    #[test]
    fn should_let_a_scheduled_send_go_out_or_be_cancelled() {
        assert!(EmailEvent::Scheduled.can_transition_to(EmailEvent::Sent));
        assert!(EmailEvent::Scheduled.can_transition_to(EmailEvent::Canceled));
    }

    #[test]
    fn should_not_let_a_sent_message_be_cancelled() {
        assert!(!EmailEvent::Sent.can_transition_to(EmailEvent::Canceled));
    }

    #[test]
    fn should_not_let_a_delivered_message_bounce() {
        assert!(!EmailEvent::Delivered.can_transition_to(EmailEvent::Bounced));
    }

    #[test]
    fn should_let_a_delivered_message_be_opened_clicked_or_complained_about() {
        for next in [
            EmailEvent::Opened,
            EmailEvent::Clicked,
            EmailEvent::Complained,
        ] {
            assert!(EmailEvent::Delivered.can_transition_to(next));
        }
    }

    #[test]
    fn should_let_a_delay_repeat() {
        assert!(EmailEvent::DeliveryDelayed.can_transition_to(EmailEvent::DeliveryDelayed));
    }

    #[test]
    fn should_let_an_open_repeat_and_a_click_follow_it() {
        assert!(EmailEvent::Opened.can_transition_to(EmailEvent::Opened));
        assert!(EmailEvent::Opened.can_transition_to(EmailEvent::Clicked));
        assert!(EmailEvent::Clicked.can_transition_to(EmailEvent::Opened));
    }

    #[test]
    fn should_never_move_a_message_backwards() {
        let backwards = [
            (EmailEvent::Sent, EmailEvent::Scheduled),
            (EmailEvent::Sent, EmailEvent::Queued),
            (EmailEvent::Delivered, EmailEvent::Sent),
            (EmailEvent::Opened, EmailEvent::Delivered),
            (EmailEvent::DeliveryDelayed, EmailEvent::Sent),
        ];

        for (from, to) in backwards {
            assert!(
                !from.can_transition_to(to),
                "{from} should not be able to go back to {to}"
            );
        }
    }

    #[test]
    fn should_not_let_any_event_follow_itself_unless_it_repeats_in_reality() {
        let repeatable = [
            EmailEvent::DeliveryDelayed,
            EmailEvent::Opened,
            EmailEvent::Clicked,
        ];

        for event in ALL_EVENTS {
            let allowed = event.can_transition_to(event);

            assert_eq!(
                allowed,
                repeatable.contains(&event),
                "{event} following itself should be {}",
                repeatable.contains(&event)
            );
        }
    }
}
