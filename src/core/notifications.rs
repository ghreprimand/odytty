// SPDX-License-Identifier: GPL-3.0-only
//! Bounded, presentation-neutral OSC notification and progress state.
//!
//! Terminal output is untrusted. These values are advisory sidecars only: they
//! never enter the grid, generate host input, open links, or persist in a
//! snapshot. The native layer owns focus policy, expiry, rate limiting, and any
//! platform notification attempt.

/// Maximum total wire bytes accepted for one notification request.
pub const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 1024;

/// Maximum queued notification requests between native-layer drains.
pub const MAX_PENDING_NOTIFICATIONS: usize = 8;

/// The supported notification spelling that produced an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSource {
    /// iTerm2-style `OSC 9 ; message`.
    Osc9,
    /// rxvt-style `OSC 777 ; notify ; title ; body`.
    Osc777,
}

/// Sanitized terminal notification request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalNotification {
    pub source: NotificationSource,
    pub title: Option<String>,
    pub body: String,
}

/// Supported `OSC 9 ; 4` progress presentation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressKind {
    Normal,
    Error,
    Indeterminate,
    Paused,
}

/// Bounded progress state. `value` exists only for determinate states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalProgress {
    pub kind: ProgressKind,
    pub value: Option<u8>,
}

pub(super) fn parse_osc9(parts: &[&[u8]]) -> Osc9Request {
    if parts.first().is_some_and(|part| *part == b"4") {
        return parse_progress(&parts[1..]).map_or(Osc9Request::Ignored, Osc9Request::Progress);
    }
    sanitize_joined(parts, MAX_NOTIFICATION_PAYLOAD_BYTES).map_or(Osc9Request::Ignored, |body| {
        Osc9Request::Notification(TerminalNotification {
            source: NotificationSource::Osc9,
            title: None,
            body,
        })
    })
}

pub(super) fn parse_osc777(parts: &[&[u8]]) -> Option<TerminalNotification> {
    if parts.first().copied() != Some(b"notify".as_slice()) {
        return None;
    }
    let payload = &parts[1..];
    if joined_len(payload)? > MAX_NOTIFICATION_PAYLOAD_BYTES {
        return None;
    }
    let title = payload
        .first()
        .and_then(|raw| sanitize_component(raw))
        .filter(|value| !value.is_empty());
    let body = sanitize_joined(
        payload.get(1..).unwrap_or_default(),
        MAX_NOTIFICATION_PAYLOAD_BYTES,
    )
    .or_else(|| title.clone())?;
    Some(TerminalNotification {
        source: NotificationSource::Osc777,
        title,
        body,
    })
}

pub(super) enum Osc9Request {
    Ignored,
    Notification(TerminalNotification),
    Progress(Option<TerminalProgress>),
}

fn parse_progress(parts: &[&[u8]]) -> Option<Option<TerminalProgress>> {
    let state = parse_ascii_u8(parts.first()?)?;
    match state {
        0 if parts.len() == 1 => Some(None),
        1 | 2 | 4 if parts.len() == 2 => {
            let value = parse_ascii_u8(parts[1])?;
            if value > 100 {
                return None;
            }
            let kind = match state {
                1 => ProgressKind::Normal,
                2 => ProgressKind::Error,
                4 => ProgressKind::Paused,
                _ => unreachable!(),
            };
            Some(Some(TerminalProgress {
                kind,
                value: Some(value),
            }))
        }
        3 if parts.len() == 1 => Some(Some(TerminalProgress {
            kind: ProgressKind::Indeterminate,
            value: None,
        })),
        _ => None,
    }
}

fn parse_ascii_u8(raw: &[u8]) -> Option<u8> {
    if raw.is_empty() || !raw.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(raw).ok()?.parse().ok()
}

fn sanitize_joined(parts: &[&[u8]], cap: usize) -> Option<String> {
    if parts.is_empty() || joined_len(parts)? > cap {
        return None;
    }
    let mut raw = Vec::with_capacity(joined_len(parts)?);
    for (index, part) in parts.iter().enumerate() {
        if index != 0 {
            raw.push(b';');
        }
        raw.extend_from_slice(part);
    }
    sanitize_component(&raw).filter(|value| !value.is_empty())
}

fn joined_len(parts: &[&[u8]]) -> Option<usize> {
    parts
        .iter()
        .try_fold(parts.len().saturating_sub(1), |sum, part| {
            sum.checked_add(part.len())
        })
}

fn sanitize_component(raw: &[u8]) -> Option<String> {
    let value: String = String::from_utf8_lossy(raw)
        .chars()
        .filter(|ch| !ch.is_control())
        .collect();
    Some(value.trim().to_owned())
}
