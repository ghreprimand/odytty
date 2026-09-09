// SPDX-License-Identifier: GPL-3.0-only
//! Event-loop ownership bridge for the opt-in local automation endpoint.

use super::{App, MultiWindowHost, UserEvent};
use crate::automation::dispatch::MAX_PER_DISPATCH;
#[cfg(test)]
use crate::automation::protocol::ObjectStatus;
use crate::automation::protocol::{
    Action, ErrorCode, MAX_OBJECTS, ObjectId, ObjectKind, Reply, Request,
};
use crate::native::automation::ReconcileOutcome;
use crate::native::quick_terminal::QuickVisibility;

impl MultiWindowHost {
    pub(super) fn service_automation_endpoint(&mut self) {
        let enabled = self
            .windows
            .first()
            .is_some_and(App::automation_endpoint_enabled);
        let ready = self.first_usable_frame_ready();
        let proxy = self.automation_proxy.clone();
        let outcome = self.automation.reconcile(enabled, ready, move || {
            proxy
                .as_ref()
                .is_some_and(|proxy| proxy.send_event(UserEvent::AutomationWake).is_ok())
        });
        match outcome {
            ReconcileOutcome::Unchanged => {}
            ReconcileOutcome::Started(endpoint) => {
                tracing::info!(%endpoint, "local automation endpoint enabled");
                if let Some(app) = self.windows.first_mut() {
                    app.automation_notice(format!("Local automation enabled at {endpoint}"), false);
                }
            }
            ReconcileOutcome::Stopped => {
                tracing::info!("local automation endpoint disabled");
                if let Some(app) = self.windows.first_mut() {
                    app.automation_notice("Local automation disabled.".to_owned(), false);
                }
            }
            ReconcileOutcome::Unavailable(reason) => {
                tracing::warn!(%reason, "local automation endpoint unavailable");
                if let Some(app) = self.windows.first_mut() {
                    #[cfg(target_os = "linux")]
                    let message = "Local automation unavailable; XDG_RUNTIME_DIR must name an owner-private runtime directory.";
                    #[cfg(target_os = "macos")]
                    let message = "Local automation unavailable; check the OdyTTY log for the owner-private state-directory error.";
                    #[cfg(windows)]
                    let message = "Local automation unavailable; check the OdyTTY log for the named-pipe security or bind error.";
                    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
                    let message = "Local automation is unavailable on this platform.";
                    app.automation_notice(message.to_owned(), true);
                }
            }
        }
    }

    pub(super) fn dispatch_automation(&mut self) {
        let Some(instance) = self.automation.instance() else {
            return;
        };
        // Move the runtime aside so the queue callback can mutably route over
        // the windows without aliasing the runtime borrow. It stays alive for
        // the complete dispatch and is restored before any subsequent turn.
        let runtime = std::mem::take(&mut self.automation);
        let handled = runtime.dispatch(|request| self.apply_automation_request(instance, request));
        self.automation = runtime;
        if handled == MAX_PER_DISPATCH
            && let Some(proxy) = self.automation_proxy.as_ref()
        {
            let _ = proxy.send_event(UserEvent::AutomationWake);
        }
    }

    pub(super) fn apply_automation_request(
        &mut self,
        instance: [u8; 16],
        request: Request,
    ) -> Reply {
        let structural = self
            .windows
            .first()
            .is_some_and(App::automation_endpoint_enabled);
        if !structural && !request.action.is_read_only() {
            return Reply::Error(ErrorCode::PermissionDenied);
        }
        match request.action {
            Action::Capabilities => Reply::Capabilities {
                structural_control: structural,
            },
            Action::List => self.automation_list(instance),
            Action::Status { target } => {
                if target.instance != instance {
                    return Reply::Error(ErrorCode::StaleIdentity);
                }
                let hidden_quick = self.hidden_quick_window();
                self.windows
                    .iter()
                    .find_map(|app| {
                        app.automation_status(
                            instance,
                            hidden_quick == Some(app.process_window_id()),
                            target.kind,
                            target.serial,
                        )
                    })
                    .map_or(Reply::Error(ErrorCode::StaleIdentity), |object| {
                        Reply::Objects(vec![object])
                    })
            }
            Action::Focus { target } => {
                // Revealing the quick terminal needs the event loop, which the
                // dispatch turn does not hold; a focus reply must never claim
                // success while the native window stays hidden.
                let hidden_quick = self.hidden_quick_window();
                self.with_automation_target(instance, target, |app| {
                    if hidden_quick == Some(app.process_window_id()) {
                        return Reply::Error(ErrorCode::Unavailable);
                    }
                    if app.automation_focus(target.kind, target.serial) {
                        Reply::Applied(target)
                    } else {
                        Reply::Error(ErrorCode::StaleIdentity)
                    }
                })
            }
            Action::OpenProfile { window, name } => {
                self.with_automation_target(instance, window, |app| {
                    app.automation_open_profile(&name)
                        .map(|serial| {
                            Reply::Applied(ObjectId {
                                instance,
                                kind: ObjectKind::Tab,
                                serial,
                            })
                        })
                        .unwrap_or_else(Reply::Error)
                })
            }
            Action::CreateTab { window } => self.with_automation_target(instance, window, |app| {
                app.automation_create_tab()
                    .map(|serial| {
                        Reply::Applied(ObjectId {
                            instance,
                            kind: ObjectKind::Tab,
                            serial,
                        })
                    })
                    .unwrap_or_else(Reply::Error)
            }),
            Action::CreateWorkspace { window, name } => {
                self.with_automation_target(instance, window, |app| {
                    app.automation_create_workspace(&name)
                        .map(|serial| {
                            Reply::Applied(ObjectId {
                                instance,
                                kind: ObjectKind::Workspace,
                                serial,
                            })
                        })
                        .unwrap_or_else(Reply::Error)
                })
            }
            Action::Split { pane, direction } => {
                self.with_automation_target(instance, pane, |app| {
                    app.automation_split(pane.serial, direction)
                        .map(|serial| {
                            Reply::Applied(ObjectId {
                                instance,
                                kind: ObjectKind::Pane,
                                serial,
                            })
                        })
                        .unwrap_or_else(Reply::Error)
                })
            }
            Action::Rename { target, name } => {
                self.with_automation_target(instance, target, |app| {
                    app.automation_rename(target.kind, target.serial, &name)
                        .map(|()| Reply::Applied(target))
                        .unwrap_or_else(Reply::Error)
                })
            }
        }
    }

    fn automation_list(&self, instance: [u8; 16]) -> Reply {
        let limit = MAX_OBJECTS.saturating_add(1);
        let hidden_quick = self.hidden_quick_window();
        let mut objects = Vec::with_capacity(limit);
        for app in &self.windows {
            let remaining = limit - objects.len();
            objects.extend(app.automation_objects_bounded(
                instance,
                hidden_quick == Some(app.process_window_id()),
                remaining,
            ));
            if objects.len() == limit {
                break;
            }
        }
        if objects.len() > MAX_OBJECTS {
            Reply::Error(ErrorCode::TooLarge)
        } else {
            Reply::Objects(objects)
        }
    }

    #[cfg(test)]
    pub(super) fn automation_objects(&self, instance: [u8; 16]) -> Vec<ObjectStatus> {
        let hidden_quick = self.hidden_quick_window();
        self.windows
            .iter()
            .flat_map(|app| {
                app.automation_objects(instance, hidden_quick == Some(app.process_window_id()))
            })
            .collect()
    }

    fn hidden_quick_window(&self) -> Option<crate::native::window_owner::ProcessWindowId> {
        self.quick
            .identity()
            .filter(|_| self.quick.visibility() == QuickVisibility::Hidden)
            .map(|identity| identity.window())
    }

    fn with_automation_target(
        &mut self,
        instance: [u8; 16],
        target: ObjectId,
        apply: impl FnOnce(&mut App) -> Reply,
    ) -> Reply {
        if target.instance != instance {
            return Reply::Error(ErrorCode::StaleIdentity);
        }
        let Some(index) = self
            .windows
            .iter()
            .position(|app| app.automation_owns(target.kind, target.serial))
        else {
            return Reply::Error(ErrorCode::StaleIdentity);
        };
        // Every caller mutates structure; the owning window's interactive
        // guards apply exactly as they do for the keyboard ladder.
        if self.windows[index].automation_interaction_busy() {
            return Reply::Error(ErrorCode::Busy);
        }
        apply(&mut self.windows[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::protocol::{Reply, Request, SplitDirection, VERSION};
    use crate::native::app::multi_window_host::tests::{headless, host_of};
    use crate::native::quick_terminal::QuickTerminalIdentity;
    use std::sync::{Arc, Mutex};

    fn enabled_host() -> (MultiWindowHost, [u8; 16]) {
        let mut host = host_of(vec![headless()]);
        host.windows[0].settings.automation_endpoint = true;
        (host, [0x42; 16])
    }

    fn object(host: &MultiWindowHost, instance: [u8; 16], kind: ObjectKind) -> ObjectId {
        host.automation_objects(instance)
            .into_iter()
            .find(|object| object.id.kind == kind)
            .expect("object of the requested kind")
            .id
    }

    fn apply(host: &mut MultiWindowHost, instance: [u8; 16], action: Action) -> Reply {
        host.apply_automation_request(
            instance,
            Request {
                version: VERSION,
                request_id: 1,
                action,
            },
        )
    }

    fn split_headless(app: &mut App) {
        let dims = crate::core::Dimensions::new(80, 24);
        app.seed_headless_split_pane_for_test(
            true,
            Arc::new(Mutex::new(crate::core::Terminal::new(
                dims.columns,
                dims.rows,
            ))),
            Arc::new(Mutex::new(Box::new(std::io::sink()))),
            dims,
        );
    }

    #[test]
    fn direct_status_matches_the_projection_for_every_object() {
        let (mut host, instance) = enabled_host();
        split_headless(&mut host.windows[0]);
        let projected = host.automation_objects(instance);
        assert!(projected.len() >= 5, "window, workspace, tab, two panes");
        for expected in projected {
            let reply = apply(
                &mut host,
                instance,
                Action::Status {
                    target: expected.id,
                },
            );
            assert_eq!(reply, Reply::Objects(vec![expected]));
        }
        let missing = ObjectId {
            instance,
            kind: ObjectKind::Tab,
            serial: u64::MAX,
        };
        assert_eq!(
            apply(&mut host, instance, Action::Status { target: missing }),
            Reply::Error(ErrorCode::StaleIdentity)
        );
    }

    #[test]
    fn mutations_are_busy_while_an_overlay_search_or_modal_owns_interaction() {
        let (mut host, instance) = enabled_host();
        let tab = object(&host, instance, ObjectKind::Tab);
        let window = object(&host, instance, ObjectKind::Window);
        let rename = Action::Rename {
            target: tab,
            name: "later".to_owned(),
        };

        host.windows[0].open_settings_overlay_for_test();
        assert_eq!(
            apply(&mut host, instance, rename.clone()),
            Reply::Error(ErrorCode::Busy)
        );
        assert_eq!(
            apply(&mut host, instance, Action::CreateTab { window }),
            Reply::Error(ErrorCode::Busy)
        );
        assert!(
            matches!(
                apply(&mut host, instance, Action::Status { target: tab }),
                Reply::Objects(_)
            ),
            "read-only status stays available beneath an overlay"
        );
        host.windows[0].close_overlay_for_test();

        host.windows[0].open_search_for_test();
        assert_eq!(
            apply(&mut host, instance, Action::Focus { target: tab }),
            Reply::Error(ErrorCode::Busy)
        );
        host.windows[0].open_search_for_test();

        assert!(host.windows[0].begin_rename_tab_for_test(0));
        assert_eq!(
            apply(&mut host, instance, rename),
            Reply::Error(ErrorCode::Busy)
        );
    }

    #[test]
    fn focus_on_the_hidden_quick_terminal_is_refused_not_reported_applied() {
        let (mut host, instance) = enabled_host();
        let window_id = host.windows[0].process_window_id();
        host.quick
            .attach_window(QuickTerminalIdentity::new(window_id));
        let _ = host.quick.hide();
        let pane = object(&host, instance, ObjectKind::Pane);
        assert_eq!(
            apply(&mut host, instance, Action::Focus { target: pane }),
            Reply::Error(ErrorCode::Unavailable)
        );
        let Reply::Objects(rows) = apply(&mut host, instance, Action::Status { target: pane })
        else {
            panic!("status reply")
        };
        assert!(rows[0].hidden && !rows[0].focused);
    }

    /// The headless window spawns nothing, so creation routes leave the active
    /// identity unchanged; the reply must then be `unavailable`, never a
    /// pre-existing identity reported as newly created.
    #[test]
    fn creation_without_a_new_activation_reports_unavailable() {
        let (mut host, instance) = enabled_host();
        let window = object(&host, instance, ObjectKind::Window);
        let pane = object(&host, instance, ObjectKind::Pane);
        assert_eq!(
            apply(
                &mut host,
                instance,
                Action::Split {
                    pane,
                    direction: SplitDirection::Rows,
                },
            ),
            Reply::Error(ErrorCode::Unavailable)
        );
        assert_eq!(
            apply(&mut host, instance, Action::CreateTab { window }),
            Reply::Error(ErrorCode::Unavailable)
        );
        assert_eq!(
            apply(
                &mut host,
                instance,
                Action::CreateWorkspace {
                    window,
                    name: "ops".to_owned(),
                },
            ),
            Reply::Error(ErrorCode::Unavailable)
        );
    }
}
