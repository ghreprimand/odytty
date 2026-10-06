// SPDX-License-Identifier: GPL-3.0-only
//! Bounded, window-specific Hyprland placement after a tab tear-out.
use super::*;
use crate::profiles::{Json, parse_json};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

const QUERY_BUDGET: Duration = Duration::from_millis(150);
const PLACEMENT_BUDGET: Duration = Duration::from_secs(3);
const MAX_REPLY: u64 = 262_144;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::native) struct Destination {
    monitor: i32,
    workspace: i32,
    selector: Option<WorkspaceSelector>,
}

/// A bounded UTF-8 workspace selector. The length is a byte length, never a
/// scalar count. Delimiters which could escape either IPC syntax are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceSelector {
    bytes: [u8; 128],
    len: usize,
}
impl WorkspaceSelector {
    fn new(name: &str) -> Result<Self, ()> {
        if !(name == "special" || name.starts_with("special:"))
            || name.len() > 128
            || name
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, ',' | ';' | '\\' | '\'' | '"'))
        {
            return Err(());
        }
        let mut bytes = [0; 128];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self {
            bytes,
            len: name.len(),
        })
    }
    fn as_str(&self) -> Result<&str, ()> {
        std::str::from_utf8(&self.bytes[..self.len]).map_err(|_| ())
    }
}

/// Requests never name current focus and never invoke a child process.
trait HyprlandIpc {
    fn request(&mut self, command: &str) -> Result<String, ()>;
}

struct SocketIpc {
    path: PathBuf,
    deadline: Instant,
}
impl SocketIpc {
    fn from_environment(budget: Duration) -> Option<Self> {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
        let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
        if signature.is_empty() || signature.len() > 200 || signature.contains(['/', '\\']) {
            return None;
        }
        Some(Self {
            path: PathBuf::from(runtime)
                .join("hypr")
                .join(signature)
                .join(".socket.sock"),
            deadline: Instant::now() + budget,
        })
    }
}
impl HyprlandIpc for SocketIpc {
    fn request(&mut self, command: &str) -> Result<String, ()> {
        use crate::automation::unix::connection::{DeadlineStream, connect, peer_is_owner};
        let remaining = || {
            self.deadline
                .checked_duration_since(Instant::now())
                .map(|budget| budget.min(QUERY_BUDGET))
                .ok_or(())
        };
        let stream = connect(&self.path, remaining()?).map_err(|_| ())?;
        peer_is_owner(&stream).map_err(|_| ())?;
        let mut stream = DeadlineStream::new(stream, remaining()?).map_err(|_| ())?;
        stream.write_all(command.as_bytes()).map_err(|_| ())?;
        let mut bytes = Vec::new();
        stream
            .take(MAX_REPLY + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() as u64 > MAX_REPLY {
            return Err(());
        }
        String::from_utf8(bytes).map_err(|_| ())
    }
}

fn number(object: &Json, key: &str) -> Option<f64> {
    object.get(key)?.as_f64().filter(|n| n.is_finite())
}
fn integer(object: &Json, key: &str) -> Option<i32> {
    let n = number(object, key)?;
    (n.fract() == 0.0 && n >= f64::from(i32::MIN) && n <= f64::from(i32::MAX)).then_some(n as i32)
}
fn json(ipc: &mut impl HyprlandIpc, command: &str) -> Result<Json, ()> {
    parse_json(&ipc.request(command)?).map_err(|_| ())
}

fn destination(ipc: &mut impl HyprlandIpc) -> Result<Destination, ()> {
    let cursor = json(ipc, "j/cursorpos")?;
    let (x, y) = (
        number(&cursor, "x").ok_or(())?,
        number(&cursor, "y").ok_or(())?,
    );
    let monitors = json(ipc, "j/monitors")?;
    let monitors = monitors.as_array().ok_or(())?;
    if monitors.len() > 64 {
        return Err(());
    }
    for monitor in monitors {
        let (mx, my) = (
            number(monitor, "x").ok_or(())?,
            number(monitor, "y").ok_or(())?,
        );
        let (width, height) = (
            number(monitor, "width").ok_or(())?,
            number(monitor, "height").ok_or(())?,
        );
        let scale = number(monitor, "scale").ok_or(())?;
        let transform = integer(monitor, "transform").ok_or(())?;
        if !(0..=7).contains(&transform)
            || !(0.25..=8.0).contains(&scale)
            || !(1.0..=65_536.0).contains(&width)
            || !(1.0..=65_536.0).contains(&height)
        {
            return Err(());
        }
        let (width, height) = if transform % 2 == 1 {
            (height / scale, width / scale)
        } else {
            (width / scale, height / scale)
        };
        if x >= mx && x < mx + width && y >= my && y < my + height {
            let id = integer(monitor, "id").filter(|id| *id >= 0).ok_or(())?;
            if let Some(special) = monitor.get("specialWorkspace") {
                let workspace = integer(special, "id").ok_or(())?;
                if workspace < 0 {
                    let name = special.get("name").and_then(Json::as_str).ok_or(())?;
                    return Ok(Destination {
                        monitor: id,
                        workspace,
                        selector: Some(WorkspaceSelector::new(name)?),
                    });
                }
                if workspace != 0 {
                    return Err(());
                }
            }
            let workspace = integer(monitor.get("activeWorkspace").ok_or(())?, "id")
                .filter(|id| *id > 0)
                .ok_or(())?;
            return Ok(Destination {
                monitor: id,
                workspace,
                selector: None,
            });
        }
    }
    Err(())
}

pub(super) fn capture_destination() -> Option<Destination> {
    let mut ipc = SocketIpc::from_environment(Duration::from_millis(400))?;
    destination(&mut ipc).ok()
}

fn address(ipc: &mut impl HyprlandIpc, identity: &str, pid: u32) -> Result<Option<String>, ()> {
    let clients = json(ipc, "j/clients")?;
    let clients = clients.as_array().ok_or(())?;
    if clients.len() > 4096 {
        return Err(());
    }
    let mut found = None;
    for client in clients {
        if client.get("initialTitle").and_then(Json::as_str) != Some(identity)
            || number(client, "pid") != Some(f64::from(pid))
        {
            continue;
        }
        let candidate = client.get("address").and_then(Json::as_str).ok_or(())?;
        let hex = candidate.strip_prefix("0x").ok_or(())?;
        if hex.is_empty()
            || hex.len() > 16
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
            || found.is_some()
        {
            return Err(());
        }
        found = Some(candidate.to_owned());
    }
    Ok(found)
}

fn apply(ipc: &mut impl HyprlandIpc, target: Destination, identity: &str) -> Result<(), ()> {
    if !identity.starts_with("OdyTTY-transfer-")
        || identity.len() > 80
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(());
    }
    // Initial titles never change, so a closed window's recycled address cannot
    // make a later IPC effect act on an unrelated client.
    let window = format!("initialtitle:^{identity}$");
    // The captured workspace must still belong to the captured monitor.
    let workspaces = json(ipc, "j/workspaces")?;
    let workspaces = workspaces.as_array().ok_or(())?;
    let selector = match target.selector {
        Some(selector) => selector.as_str()?.to_owned(),
        None if target.workspace > 0 => target.workspace.to_string(),
        None => return Err(()),
    };
    if workspaces.len() > 4096
        || !workspaces.iter().any(|ws| {
            integer(ws, "id") == Some(target.workspace)
                && integer(ws, "monitorID") == Some(target.monitor)
                && (target.selector.is_none()
                    || ws.get("name").and_then(Json::as_str) == Some(selector.as_str()))
        })
    {
        return Err(());
    }
    dispatch(
        ipc,
        &format!("/dispatch settiled {window}"),
        &format!("/dispatch hl.dsp.window.float({{window='{window}',action='disable'}})"),
    )?;
    dispatch(
        ipc,
        &format!("/dispatch movetoworkspacesilent {},{window}", selector),
        &format!(
            "/dispatch hl.dsp.window.move({{window='{window}',workspace='{}',follow=false}})",
            selector
        ),
    )?;
    Ok(())
}

fn dispatch(ipc: &mut impl HyprlandIpc, classic: &str, lua: &str) -> Result<(), ()> {
    let response = ipc.request(classic)?;
    if response.trim() == "ok" {
        return Ok(());
    }
    // Hyprland 0.56 keeps classic dispatch for the legacy parser. Its Lua
    // parser returns this explicit, documented diagnostic before any action.
    // Never retry an ordinary refused operation with a different command.
    if response.contains("dispatch in lua is a shorthand for hl.dispatch")
        && ipc.request(lua)?.trim() == "ok"
    {
        return Ok(());
    }
    Err(())
}

pub(super) struct PendingPlacement {
    pub(super) identity: String,
    target: Destination,
    completion: Option<Receiver<bool>>,
}

impl App {
    pub(super) fn prepare_tear_out_placement(&mut self, target: Option<Destination>) {
        if let Some(target) = target {
            // Stable creation identity, held until mapping/placement completes.
            self.pending_tear_out_placement = Some(PendingPlacement {
                identity: format!(
                    "OdyTTY-transfer-{}-{}",
                    std::process::id(),
                    self.process_window_id.0
                ),
                target,
                completion: None,
            });
        }
    }

    pub(super) fn start_tear_out_placement(&mut self) {
        let Some(pending) = self.pending_tear_out_placement.as_mut() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let identity = pending.identity.clone();
        let target = pending.target;
        pending.completion = Some(receiver);
        let started = std::thread::Builder::new()
            .name("odytty-window-placement".into())
            .spawn(move || {
                let result = (|| {
                    let mut ipc = SocketIpc::from_environment(PLACEMENT_BUDGET).ok_or(())?;
                    let deadline = Instant::now() + PLACEMENT_BUDGET;
                    while Instant::now() < deadline {
                        if address(&mut ipc, &identity, std::process::id())?.is_some() {
                            return apply(&mut ipc, target, &identity);
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(())
                })();
                let _ = sender.send(result.is_ok());
            });
        if started.is_err() {
            self.finish_tear_out_placement(false);
        }
    }

    pub(super) fn poll_tear_out_placement(&mut self) {
        let result = self
            .pending_tear_out_placement
            .as_ref()
            .and_then(|pending| pending.completion.as_ref())
            .map(Receiver::try_recv);
        match result {
            Some(Ok(placed)) => self.finish_tear_out_placement(placed),
            Some(Err(mpsc::TryRecvError::Disconnected)) => self.finish_tear_out_placement(false),
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
    }

    pub(super) fn finish_tear_out_placement(&mut self, placed: bool) {
        self.pending_tear_out_placement = None;
        self.sync_active_window_title();
        if !placed {
            tracing::debug!("Hyprland tab placement unavailable; compositor placement retained");
            self.raise_open_notice("The tab moved; Hyprland placement was unavailable".to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Fake {
        replies: VecDeque<(&'static str, &'static str)>,
        commands: Vec<String>,
    }
    impl HyprlandIpc for Fake {
        fn request(&mut self, command: &str) -> Result<String, ()> {
            self.commands.push(command.to_owned());
            let (expected, response) = self.replies.pop_front().ok_or(())?;
            assert_eq!(command, expected);
            Ok(response.into())
        }
    }
    fn fake(replies: &[(&'static str, &'static str)]) -> Fake {
        Fake {
            replies: replies.iter().copied().collect(),
            commands: Vec::new(),
        }
    }
    // Project-authored, numeric compositor fixtures. No machine/provider identity.
    const MONITORS: &str = r#"[{"id":0,"x":0,"y":0,"width":1920,"height":1080,"scale":1.25,"transform":0,"activeWorkspace":{"id":2}},{"id":1,"x":1536,"y":0,"width":2560,"height":1440,"scale":1.667,"transform":0,"activeWorkspace":{"id":5}},{"id":3,"x":0,"y":-1080,"width":1920,"height":1080,"scale":1,"transform":0,"activeWorkspace":{"id":7}}]"#;
    #[test]
    fn release_time_global_cursor_selects_mixed_scale_and_negative_monitors() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":2000,"y":500}"#),
            ("j/monitors", MONITORS),
        ]);
        assert_eq!(
            destination(&mut ipc),
            Ok(Destination {
                monitor: 1,
                workspace: 5,
                selector: None
            })
        );
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":1200,"y":-800}"#),
            ("j/monitors", MONITORS),
        ]);
        assert_eq!(
            destination(&mut ipc),
            Ok(Destination {
                monitor: 3,
                workspace: 7,
                selector: None
            })
        );
    }
    #[test]
    fn visible_special_workspace_is_the_release_destination_not_the_hidden_regular_one() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":20,"y":20}"#),
            (
                "j/monitors",
                r#"[{"id":1,"x":0,"y":0,"width":1920,"height":1080,"scale":1,"transform":0,"activeWorkspace":{"id":5},"specialWorkspace":{"id":-5,"name":"special:tools"}}]"#,
            ),
        ]);
        assert_eq!(destination(&mut ipc).unwrap().workspace, -5);
    }

    #[test]
    fn monitor_rotation_and_bounds_are_validated() {
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":800,"y":1400}"#),
            (
                "j/monitors",
                r#"[{"id":2,"x":0,"y":0,"width":1920,"height":1080,"scale":1,"transform":1,"activeWorkspace":{"id":8}}]"#,
            ),
        ]);
        assert_eq!(
            destination(&mut ipc),
            Ok(Destination {
                monitor: 2,
                workspace: 8,
                selector: None
            })
        );
        let mut ipc = fake(&[
            ("j/cursorpos", r#"{"x":0,"y":0}"#),
            (
                "j/monitors",
                r#"[{"id":2,"x":0,"y":0,"width":1920,"height":1080,"scale":0,"transform":0,"activeWorkspace":{"id":8}}]"#,
            ),
        ]);
        assert_eq!(destination(&mut ipc), Err(()));
    }
    #[test]
    fn creation_identity_and_pid_both_select_the_window_not_current_focus() {
        let mut ipc = fake(&[(
            "j/clients",
            r#"[{"pid":10,"initialTitle":"OdyTTY-transfer-test","address":"0x10"},{"pid":11,"initialTitle":"OdyTTY-transfer-test","address":"0x20"}]"#,
        )]);
        assert_eq!(
            address(&mut ipc, "OdyTTY-transfer-test", 11),
            Ok(Some("0x20".into()))
        );
        let mut ipc = fake(&[(
            "j/clients",
            r#"[{"pid":11,"initialTitle":"OdyTTY-transfer-test","address":"0x20;exec"}]"#,
        )]);
        assert_eq!(address(&mut ipc, "OdyTTY-transfer-test", 11), Err(()));
    }
    #[test]
    fn placement_tiles_and_moves_only_the_creation_identity_to_the_captured_workspace() {
        let mut ipc = fake(&[
            ("j/workspaces", r#"[{"id":5,"monitorID":1}]"#),
            (
                "/dispatch settiled initialtitle:^OdyTTY-transfer-test$",
                "ok",
            ),
            (
                "/dispatch movetoworkspacesilent 5,initialtitle:^OdyTTY-transfer-test$",
                "ok",
            ),
        ]);
        assert_eq!(
            apply(
                &mut ipc,
                Destination {
                    monitor: 1,
                    workspace: 5,
                    selector: None
                },
                "OdyTTY-transfer-test"
            ),
            Ok(())
        );
        assert_eq!(ipc.commands.len(), 3);
    }
    #[test]
    fn placement_effects_use_the_immutable_creation_identity_not_a_reusable_address() {
        let mut ipc = fake(&[
            ("j/workspaces", r#"[{"id":5,"monitorID":1}]"#),
            (
                "/dispatch settiled initialtitle:^OdyTTY-transfer-test$",
                "ok",
            ),
            (
                "/dispatch movetoworkspacesilent 5,initialtitle:^OdyTTY-transfer-test$",
                "ok",
            ),
        ]);
        assert_eq!(
            apply(
                &mut ipc,
                Destination {
                    monitor: 1,
                    workspace: 5,
                    selector: None
                },
                "OdyTTY-transfer-test"
            ),
            Ok(())
        );
        assert!(
            ipc.commands
                .iter()
                .all(|command| !command.contains("address:")),
            "closing the new window can recycle its address before a later request"
        );
    }

    #[test]
    fn lua_dispatcher_mode_uses_explicit_window_and_no_follow() {
        let mut ipc = fake(&[
            ("j/workspaces", r#"[{"id":5,"monitorID":1}]"#),
            (
                "/dispatch settiled initialtitle:^OdyTTY-transfer-test$",
                "syntax error: dispatch in lua is a shorthand for hl.dispatch",
            ),
            (
                "/dispatch hl.dsp.window.float({window='initialtitle:^OdyTTY-transfer-test$',action='disable'})",
                "ok",
            ),
            (
                "/dispatch movetoworkspacesilent 5,initialtitle:^OdyTTY-transfer-test$",
                "syntax error: dispatch in lua is a shorthand for hl.dispatch",
            ),
            (
                "/dispatch hl.dsp.window.move({window='initialtitle:^OdyTTY-transfer-test$',workspace='5',follow=false})",
                "ok",
            ),
        ]);
        assert_eq!(
            apply(
                &mut ipc,
                Destination {
                    monitor: 1,
                    workspace: 5,
                    selector: None
                },
                "OdyTTY-transfer-test"
            ),
            Ok(())
        );
        assert_eq!(ipc.commands.len(), 5);
    }

    #[test]
    fn special_workspace_placement_uses_the_named_selector_and_rejects_delimiters() {
        let target = Destination {
            monitor: 1,
            workspace: -5,
            selector: Some(WorkspaceSelector::new("special:tools").unwrap()),
        };
        let mut ipc = fake(&[
            (
                "j/workspaces",
                r#"[{"id":-5,"monitorID":1,"name":"special:tools"}]"#,
            ),
            (
                "/dispatch settiled initialtitle:^OdyTTY-transfer-test$",
                "ok",
            ),
            (
                "/dispatch movetoworkspacesilent special:tools,initialtitle:^OdyTTY-transfer-test$",
                "ok",
            ),
        ]);
        assert_eq!(apply(&mut ipc, target, "OdyTTY-transfer-test"), Ok(()));
        for name in [
            "special:bad,name",
            "special:bad;name",
            "special:bad'name",
            "special:bad\\name",
            "special:bad\nname",
        ] {
            assert!(WorkspaceSelector::new(name).is_err());
        }
        assert!(WorkspaceSelector::new(&format!("special:{}", "a".repeat(129))).is_err());
        let mut ipc = fake(&[(
            "j/workspaces",
            r#"[{"id":-5,"monitorID":1,"name":"special:other"}]"#,
        )]);
        assert_eq!(apply(&mut ipc, target, "OdyTTY-transfer-test"), Err(()));
        assert_eq!(ipc.commands.len(), 1);
    }

    #[test]
    fn stale_workspace_and_failed_dispatch_do_not_issue_followup_effects() {
        let mut ipc = fake(&[("j/workspaces", r#"[{"id":5,"monitorID":0}]"#)]);
        assert_eq!(
            apply(
                &mut ipc,
                Destination {
                    monitor: 1,
                    workspace: 5,
                    selector: None
                },
                "OdyTTY-transfer-test"
            ),
            Err(())
        );
        assert_eq!(ipc.commands, vec!["j/workspaces"]);
        let mut ipc = fake(&[
            ("j/workspaces", r#"[{"id":5,"monitorID":1}]"#),
            (
                "/dispatch settiled initialtitle:^OdyTTY-transfer-test$",
                "refused",
            ),
        ]);
        assert_eq!(
            apply(
                &mut ipc,
                Destination {
                    monitor: 1,
                    workspace: 5,
                    selector: None
                },
                "OdyTTY-transfer-test"
            ),
            Err(())
        );
        assert_eq!(ipc.commands.len(), 2);
    }
}
