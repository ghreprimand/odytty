// SPDX-License-Identifier: GPL-3.0-only
//! Headless notification ownership and focus-policy seams.

use super::*;

impl App {
    pub(in crate::native) fn drain_all_notifications_for_test(
        &mut self,
        now: std::time::Instant,
        window_focused: bool,
    ) -> (bool, bool, Option<bool>) {
        let sweep = self.sessions.drain_notifications(
            now,
            window_focused,
            self.settings.notifications.shows_in_app(),
        );
        (
            sweep.changed,
            sweep.background_request,
            sweep.command_completion,
        )
    }

    pub(in crate::native) fn pane_attention_for_test(
        &self,
        token: usize,
    ) -> (bool, bool, bool, Option<crate::core::TerminalProgress>) {
        self.sessions
            .get(crate::native::session::SessionToken(token as u64))
            .map(|session| {
                (
                    session.attention.unread,
                    session.attention.completed,
                    session.attention.failed,
                    session.attention.progress,
                )
            })
            .unwrap_or_default()
    }
}
