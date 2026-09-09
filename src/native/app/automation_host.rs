// SPDX-License-Identifier: GPL-3.0-only
//! Event-loop ownership bridge for the opt-in local automation endpoint.

use super::{App, MultiWindowHost, UserEvent};
use crate::automation::dispatch::MAX_PER_DISPATCH;
use crate::automation::protocol::{
    Action, ErrorCode, MAX_OBJECTS, ObjectId, ObjectKind, ObjectStatus, Reply, Request,
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
                    let message = "Local automation is unavailable on Windows until named-pipe support is enabled.";
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
                match self
                    .automation_objects(instance)
                    .into_iter()
                    .find(|object| object.id == target)
                {
                    Some(object) => Reply::Objects(vec![object]),
                    None => Reply::Error(ErrorCode::StaleIdentity),
                }
            }
            Action::Focus { target } => self.with_automation_target(instance, target, |app| {
                if app.automation_focus(target.kind, target.serial) {
                    Reply::Applied(target)
                } else {
                    Reply::Error(ErrorCode::StaleIdentity)
                }
            }),
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
        apply(&mut self.windows[index])
    }
}
