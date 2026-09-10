// SPDX-License-Identifier: GPL-3.0-only
//! Typed control CLI. No shell parsing or automatic mutation retries.

use std::ffi::OsString;
use std::path::PathBuf;
#[cfg(unix)]
use std::time::{Duration, Instant};

use super::protocol::{
    self, Action, ErrorCode, ObjectId, ObjectKind, Reply, Request, Response, SplitDirection,
    VERSION,
};

pub const USAGE: &str = "Usage: odytty control [--endpoint PATH] [--json] COMMAND [ARGS]\n\
Commands: capabilities | list | status ID | focus ID | open-profile WINDOW NAME |\n\
          create-tab WINDOW | create-workspace WINDOW NAME |\n\
          split PANE columns|rows | rename ID NAME | quick-terminal toggle\n\
IDs: 32-hex-instance:window|workspace|tab|pane:decimal-serial\n\
Only quick-terminal toggle can discover an endpoint; every other command requires --endpoint.\n\
Discovery skips stale sockets; it refuses when any live socket cannot be classified.\n\
The endpoint must be explicitly enabled by its owner. No terminal input or content reads.\n\
Unix requires an owner-private directory; Windows requires a local OdyTTY named pipe.\n\
A lost mutation reply has an unknown outcome; do not retry automatically.\n";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Request {
        endpoint: Option<PathBuf>,
        json: bool,
        request: Request,
    },
}

/// Only consumes `control`; unrelated CLI arguments retain their existing path.
/// Endpoint paths retain OsString bytes. Every other argument must be Unicode.
pub fn parse(args: &[OsString]) -> Result<Option<Command>, ErrorCode> {
    if args.first().is_none_or(|arg| arg != "control") {
        return Ok(None);
    }
    if args.len() == 2 && (args[1] == "--help" || args[1] == "-h") {
        return Ok(Some(Command::Help));
    }
    let mut endpoint = None;
    let mut json = false;
    let mut index = 1;
    while let Some(arg) = args.get(index).and_then(|arg| arg.to_str()) {
        match arg {
            "--endpoint" if endpoint.is_none() => {
                index += 1;
                endpoint = Some(PathBuf::from(
                    args.get(index).ok_or(ErrorCode::InvalidRequest)?,
                ));
            }
            "--json" if !json => json = true,
            _ => break,
        }
        index += 1;
    }
    let endpoint_was_supplied = endpoint.is_some();
    let endpoint = endpoint.filter(|path| !path.as_os_str().is_empty());
    if endpoint_was_supplied && endpoint.is_none() {
        return Err(ErrorCode::InvalidRequest);
    }
    let words: Vec<&str> = args[index..]
        .iter()
        .map(|arg| arg.to_str().ok_or(ErrorCode::InvalidRequest))
        .collect::<Result<_, _>>()?;
    let action = match words.as_slice() {
        ["capabilities"] => Action::Capabilities,
        ["list"] => Action::List,
        ["status", id] => Action::Status {
            target: parse_id(id)?,
        },
        ["focus", id] => Action::Focus {
            target: parse_id(id)?,
        },
        ["open-profile", id, name] => Action::OpenProfile {
            window: parse_id(id)?,
            name: (*name).to_owned(),
        },
        ["create-tab", id] => Action::CreateTab {
            window: parse_id(id)?,
        },
        ["create-workspace", id, name] => Action::CreateWorkspace {
            window: parse_id(id)?,
            name: (*name).to_owned(),
        },
        ["split", id, axis] => Action::Split {
            pane: parse_id(id)?,
            direction: match *axis {
                "columns" => SplitDirection::Columns,
                "rows" => SplitDirection::Rows,
                _ => return Err(ErrorCode::InvalidRequest),
            },
        },
        ["rename", id, name] => Action::Rename {
            target: parse_id(id)?,
            name: (*name).to_owned(),
        },
        ["quick-terminal", "toggle"] => Action::QuickTerminalToggle,
        _ => return Err(ErrorCode::InvalidRequest),
    };
    if endpoint.is_none() && !matches!(action, Action::QuickTerminalToggle) {
        return Err(ErrorCode::InvalidRequest);
    }
    let request = Request {
        version: VERSION,
        request_id: 1,
        action,
    };
    protocol::encode(&request)?;
    Ok(Some(Command::Request {
        endpoint,
        json,
        request,
    }))
}

fn parse_id(text: &str) -> Result<ObjectId, ErrorCode> {
    let mut fields = text.split(':');
    let entropy = fields.next().ok_or(ErrorCode::InvalidRequest)?;
    if entropy.len() != 32 || !entropy.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ErrorCode::InvalidRequest);
    }
    let mut instance = [0; 16];
    for (index, byte) in instance.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&entropy[index * 2..index * 2 + 2], 16)
            .map_err(|_| ErrorCode::InvalidRequest)?;
    }
    let kind = match fields.next() {
        Some("window") => ObjectKind::Window,
        Some("workspace") => ObjectKind::Workspace,
        Some("tab") => ObjectKind::Tab,
        Some("pane") => ObjectKind::Pane,
        _ => return Err(ErrorCode::InvalidRequest),
    };
    let serial = fields
        .next()
        .filter(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        .ok_or(ErrorCode::InvalidRequest)?
        .parse()
        .map_err(|_| ErrorCode::InvalidRequest)?;
    if fields.next().is_some() {
        return Err(ErrorCode::InvalidRequest);
    }
    Ok(ObjectId {
        instance,
        kind,
        serial,
    })
}

pub fn format_id(id: &ObjectId) -> String {
    let entropy: String = id
        .instance
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let kind = match id.kind {
        ObjectKind::Window => "window",
        ObjectKind::Workspace => "workspace",
        ObjectKind::Tab => "tab",
        ObjectKind::Pane => "pane",
    };
    format!("{entropy}:{kind}:{}", id.serial)
}

/// Only fixed protocol labels, booleans, integers and formatted IDs are emitted.
/// No title, path, profile name or untrusted error text reaches either format.
pub fn format_response(response: &Response, json: bool) -> String {
    let request_id = response.request_id;
    if json {
        let body = match &response.reply {
            Reply::Capabilities {
                structural_control,
                quick_terminal_toggle,
            } => format!(
                "\"capabilities\":{{\"protocol_version\":{VERSION},\"structural_control\":{structural_control},\"quick_terminal_toggle\":{quick_terminal_toggle},\"terminal_input\":false,\"terminal_content\":false}}"
            ),
            Reply::Objects(objects) => {
                let rows: Vec<String> = objects
                    .iter()
                    .map(|object| {
                        let parent = object
                            .parent
                            .as_ref()
                            .map(|id| format!("\"{}\"", format_id(id)))
                            .unwrap_or_else(|| "null".into());
                        format!(
                            "{{\"id\":\"{}\",\"parent\":{parent},\"focused\":{},\"hidden\":{}}}",
                            format_id(&object.id),
                            object.focused,
                            object.hidden
                        )
                    })
                    .collect();
                format!("\"objects\":[{}]", rows.join(","))
            }
            Reply::Applied(id) => format!("\"applied\":\"{}\"", format_id(id)),
            Reply::Accepted => "\"accepted\":true".to_owned(),
            Reply::Error(error) => format!("\"error\":\"{error}\""),
        };
        format!("{{\"request_id\":{request_id},{body}}}\n")
    } else {
        match &response.reply {
            Reply::Capabilities {
                structural_control,
                quick_terminal_toggle,
            } => format!(
                "protocol_version={VERSION} structural_control={structural_control} quick_terminal_toggle={quick_terminal_toggle} terminal_input=false terminal_content=false\n"
            ),
            Reply::Objects(objects) => objects
                .iter()
                .map(|object| {
                    format!(
                        "{} parent={} focused={} hidden={}\n",
                        format_id(&object.id),
                        object
                            .parent
                            .as_ref()
                            .map(format_id)
                            .unwrap_or_else(|| "none".into()),
                        object.focused,
                        object.hidden
                    )
                })
                .collect(),
            Reply::Applied(id) => format!("applied {}\n", format_id(id)),
            Reply::Accepted => "accepted\n".to_owned(),
            Reply::Error(error) => format!("error {error}\n"),
        }
    }
}

/// Returns output and process success without launching a GUI or loading
/// settings/profiles. The caller writes once and maps false to a nonzero exit.
pub fn run(command: Command) -> (String, bool) {
    let Command::Request {
        endpoint,
        json,
        request,
    } = command
    else {
        return (USAGE.into(), true);
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if endpoint.is_none() {
        return run_discovered_unix(request, json);
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let response =
        explicit_unix_response(endpoint.as_deref().expect("explicit endpoint"), &request);
    #[cfg(windows)]
    if endpoint.is_none() {
        let message = r"Windows requires --endpoint \\.\pipe\odytty-control-<pid>";
        return if json {
            (
                format!(
                    "{{\"request_id\":1,\"error\":\"unavailable\",\"message\":\"{}\"}}\n",
                    json_escape(message)
                ),
                false,
            )
        } else {
            (format!("error unavailable: {message}\n"), false)
        };
    }
    #[cfg(windows)]
    let response =
        super::windows::request(endpoint.as_deref().expect("explicit endpoint"), &request)
            .unwrap_or_else(|_| Response {
                request_id: request.request_id,
                reply: Reply::Error(if request.action.is_read_only() {
                    ErrorCode::Unavailable
                } else {
                    ErrorCode::OutcomeUnknown
                }),
            });
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let response = {
        let _ = endpoint;
        Response {
            request_id: request.request_id,
            reply: Reply::Error(ErrorCode::Unavailable),
        }
    };
    let success = !matches!(response.reply, Reply::Error(_));
    (format_response(&response, json), success)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn explicit_unix_response(endpoint: &std::path::Path, request: &Request) -> Response {
    mutation_response(request, || super::unix::request(endpoint, request))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mutation_response(
    request: &Request,
    send: impl FnOnce() -> std::io::Result<Response>,
) -> Response {
    send().unwrap_or_else(|_| Response {
        request_id: request.request_id,
        reply: Reply::Error(if request.action.is_read_only() {
            ErrorCode::Unavailable
        } else {
            ErrorCode::OutcomeUnknown
        }),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
const DISCOVERY_BUDGET: Duration = Duration::from_secs(1);

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, PartialEq, Eq)]
enum DiscoveryFailure {
    NoRuntimeOrSockets,
    NoneEligible { stale: usize },
    Multiple(Vec<PathBuf>),
    Unclassified(Vec<PathBuf>),
    EnumerationUncertain(Vec<PathBuf>),
    TooMany(Vec<PathBuf>),
    TimedOut(Vec<PathBuf>),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_discovered_unix(request: Request, json: bool) -> (String, bool) {
    debug_assert!(matches!(request.action, Action::QuickTerminalToggle));
    let started = Instant::now();
    #[cfg(target_os = "linux")]
    let candidates = super::unix::discovery_candidates(
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        started + DISCOVERY_BUDGET,
    );
    #[cfg(target_os = "macos")]
    let candidates = super::unix::discovery_candidates_in(
        &crate::logging::state_log_dir().join("control"),
        started + DISCOVERY_BUDGET,
    );
    let candidates = match candidates {
        Ok(candidates) if !candidates.is_empty() => candidates,
        Ok(_) => return format_discovery_failure(DiscoveryFailure::NoRuntimeOrSockets, json),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            return format_discovery_failure(
                DiscoveryFailure::TooMany(error.unresolved_paths().to_vec()),
                json,
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            return format_discovery_failure(
                DiscoveryFailure::TimedOut(error.unresolved_paths().to_vec()),
                json,
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return format_discovery_failure(DiscoveryFailure::NoRuntimeOrSockets, json);
        }
        Err(error) => {
            return format_discovery_failure(
                DiscoveryFailure::EnumerationUncertain(error.unresolved_paths().to_vec()),
                json,
            );
        }
    };
    let result = discover_eligible(candidates, started, |path, timeout| {
        super::unix::request_with_timeout(
            path,
            &Request {
                version: VERSION,
                request_id: request.request_id,
                action: Action::Capabilities,
            },
            timeout,
        )
    });
    let endpoint = match result {
        Ok(endpoint) => endpoint,
        Err(error) => return format_discovery_failure(error, json),
    };
    let response = explicit_unix_response(&endpoint, &request);
    let success = !matches!(response.reply, Reply::Error(_));
    (format_response(&response, json), success)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn discover_eligible(
    candidates: Vec<PathBuf>,
    started: Instant,
    mut probe: impl FnMut(&std::path::Path, Duration) -> std::io::Result<Response>,
) -> Result<PathBuf, DiscoveryFailure> {
    let mut eligible = Vec::new();
    let mut unclassified = Vec::new();
    let mut stale = 0;
    for (index, candidate) in candidates.iter().enumerate() {
        let remaining = DISCOVERY_BUDGET.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            append_unique(&mut unclassified, &candidates[index..]);
            return Err(DiscoveryFailure::Unclassified(unclassified));
        }
        match probe(candidate, remaining) {
            Ok(Response {
                reply:
                    Reply::Capabilities {
                        structural_control: true,
                        quick_terminal_toggle: true,
                    },
                ..
            }) => eligible.push(candidate.clone()),
            Ok(Response {
                reply:
                    Reply::Capabilities {
                        quick_terminal_toggle: false,
                        ..
                    },
                ..
            }) => {}
            Ok(Response {
                reply: Reply::Error(ErrorCode::VersionMismatch | ErrorCode::UnsupportedCapability),
                ..
            }) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                stale += 1;
            }
            Ok(_) | Err(_) => unclassified.push(candidate.clone()),
        }
        if started.elapsed() >= DISCOVERY_BUDGET {
            append_unique(&mut unclassified, &candidates[index..]);
            return Err(DiscoveryFailure::Unclassified(unclassified));
        }
    }
    if !unclassified.is_empty() {
        return Err(DiscoveryFailure::Unclassified(unclassified));
    }
    match eligible.len() {
        0 => Err(DiscoveryFailure::NoneEligible { stale }),
        1 => Ok(eligible.pop().expect("one eligible endpoint")),
        _ => Err(DiscoveryFailure::Multiple(eligible)),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn append_unique(paths: &mut Vec<PathBuf>, candidates: &[PathBuf]) {
    for candidate in candidates {
        if !paths.contains(candidate) {
            paths.push(candidate.clone());
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn format_discovery_failure(error: DiscoveryFailure, json: bool) -> (String, bool) {
    let message = match error {
        DiscoveryFailure::NoRuntimeOrSockets => {
            "no running OdyTTY has automation_endpoint = on".to_owned()
        }
        DiscoveryFailure::NoneEligible { stale } => format!(
            "quick_terminal is off in the running instance or the endpoint is older than this CLI; {stale} stale endpoints ignored"
        ),
        DiscoveryFailure::Multiple(paths) => format!(
            "multiple eligible OdyTTY endpoints; use --endpoint PATH: {}",
            paths
                .iter()
                .map(|path| format!("{path:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        DiscoveryFailure::Unclassified(paths) => format!(
            "OdyTTY endpoint discovery could not classify {}; no toggle was sent; use --endpoint PATH",
            paths
                .iter()
                .map(|path| format!("{path:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        DiscoveryFailure::EnumerationUncertain(paths) => format!(
            "OdyTTY endpoint directory scan could not classify {}; no toggle was sent; use --endpoint PATH",
            format_paths(&paths)
        ),
        DiscoveryFailure::TooMany(paths) => format!(
            "more than {} OdyTTY endpoint candidates below {}; use --endpoint PATH",
            super::unix::MAX_DISCOVERY_CANDIDATES,
            format_paths(&paths)
        ),
        DiscoveryFailure::TimedOut(paths) => format!(
            "OdyTTY endpoint discovery exceeded 1 second while classifying {}; no toggle was sent; use --endpoint PATH",
            format_paths(&paths)
        ),
    };
    if json {
        (
            format!(
                "{{\"request_id\":1,\"error\":\"unavailable\",\"message\":\"{}\"}}\n",
                json_escape(&message)
            ),
            false,
        )
    } else {
        (format!("error unavailable: {message}\n"), false)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn format_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        "an unknown endpoint path".to_owned()
    } else {
        paths
            .iter()
            .map(|path| format!("{path:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn json_escape(value: &str) -> String {
    value.chars().fold(String::new(), |mut output, character| {
        match character {
            '\"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            value if value <= '\u{1f}' => {
                output.push_str(&format!("\\u{:04x}", value as u32));
            }
            value => output.push(value),
        }
        output
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }
    const WINDOW: &str = "00000000000000000000000000000000:window:7";

    #[test]
    fn cli_requires_explicit_endpoint_and_exact_typed_arguments() {
        assert!(parse(&args(&["control", "list"])).is_err());
        assert!(
            parse(&args(&[
                "control",
                "--endpoint",
                "/example/control",
                "list",
                "extra"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "control",
                "--endpoint",
                "/example/control",
                "split",
                WINDOW,
                "rows"
            ]))
            .is_err()
        );
        assert!(
            parse(&args(&[
                "control",
                "--endpoint",
                "/example/control",
                "send-text",
                "echo"
            ]))
            .is_err()
        );
        assert!(parse(&args(&["--native"])).unwrap().is_none());
    }

    #[test]
    fn cli_preserves_literal_profile_argument_without_shell_expansion() {
        let name = "literal $(printf injected) ' profile";
        let Some(Command::Request { request, .. }) = parse(&args(&[
            "control",
            "--endpoint",
            "/example/control",
            "open-profile",
            WINDOW,
            name,
        ]))
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            request.action,
            Action::OpenProfile {
                window: parse_id(WINDOW).unwrap(),
                name: name.into()
            }
        );
    }

    #[test]
    fn cli_parses_quick_terminal_toggle_with_an_explicit_endpoint() {
        let Some(Command::Request { request, .. }) = parse(&args(&[
            "control",
            "--endpoint",
            "/example/control",
            "quick-terminal",
            "toggle",
        ]))
        .unwrap() else {
            panic!("expected Request")
        };
        assert_eq!(request.action, Action::QuickTerminalToggle);
    }

    #[test]
    fn only_quick_terminal_toggle_may_omit_the_endpoint() {
        let Some(Command::Request {
            endpoint, request, ..
        }) = parse(&args(&["control", "quick-terminal", "toggle"])).unwrap()
        else {
            panic!("expected Request")
        };
        assert_eq!(endpoint, None);
        assert_eq!(request.action, Action::QuickTerminalToggle);
        for command in [
            vec!["control", "capabilities"],
            vec!["control", "list"],
            vec!["control", "create-tab", WINDOW],
            vec!["control", "--endpoint", "", "quick-terminal", "toggle"],
        ] {
            assert_eq!(parse(&args(&command)), Err(ErrorCode::InvalidRequest));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn capability_response(enabled: bool) -> Response {
        Response {
            request_id: 1,
            reply: Reply::Capabilities {
                structural_control: true,
                quick_terminal_toggle: enabled,
            },
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn discovery_requires_exactly_one_eligible_endpoint() {
        let off = PathBuf::from("/runtime/control-1.sock");
        let on = PathBuf::from("/runtime/control-2.sock");
        let stale = PathBuf::from("/runtime/control-3.sock");
        let selected = discover_eligible(
            vec![off.clone(), on.clone(), stale.clone()],
            Instant::now(),
            |path, _| {
                if path == off {
                    Ok(capability_response(false))
                } else if path == on {
                    Ok(capability_response(true))
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "stale",
                    ))
                }
            },
        )
        .expect("one eligible endpoint");
        assert_eq!(selected, on);

        let first = PathBuf::from("/runtime/control-10.sock");
        let second = PathBuf::from("/runtime/control-11.sock");
        assert_eq!(
            discover_eligible(
                vec![first.clone(), second.clone()],
                Instant::now(),
                |_, _| Ok(capability_response(true)),
            ),
            Err(DiscoveryFailure::Multiple(vec![first, second]))
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn discovery_skips_and_counts_only_definitive_stale_connects() {
        let paths = (1..=2)
            .map(|pid| PathBuf::from(format!("/runtime/control-{pid}.sock")))
            .collect::<Vec<_>>();
        let mut index = 0;
        let result = discover_eligible(paths, Instant::now(), |_, _| {
            let kinds = [
                std::io::ErrorKind::ConnectionRefused,
                std::io::ErrorKind::NotFound,
            ];
            let kind = kinds[index];
            index += 1;
            Err(std::io::Error::new(kind, "failed probe"))
        });
        assert_eq!(result, Err(DiscoveryFailure::NoneEligible { stale: 2 }));
        let (message, success) = format_discovery_failure(result.unwrap_err(), false);
        assert!(!success);
        assert!(message.contains("2 stale endpoints ignored"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn one_eligible_plus_one_unclassifiable_endpoint_refuses() {
        let eligible = PathBuf::from("/runtime/control-1.sock");
        let unresolved = PathBuf::from("/runtime/control-2.sock");
        for kind in [
            std::io::ErrorKind::InvalidData,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::Other,
        ] {
            let result = discover_eligible(
                vec![eligible.clone(), unresolved.clone()],
                Instant::now(),
                |path, _| {
                    if path == eligible {
                        Ok(capability_response(true))
                    } else {
                        Err(std::io::Error::new(kind, "unclassifiable response"))
                    }
                },
            );
            assert_eq!(
                result,
                Err(DiscoveryFailure::Unclassified(vec![unresolved.clone()]))
            );
            let (message, success) = format_discovery_failure(result.unwrap_err(), false);
            assert!(!success);
            assert!(message.contains(&format!("{unresolved:?}")));
            assert!(message.contains("no toggle was sent"));
            assert!(message.contains("use --endpoint PATH"));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn one_eligible_plus_one_stale_endpoint_selects_the_eligible_one() {
        let eligible = PathBuf::from("/runtime/control-1.sock");
        let stale = PathBuf::from("/runtime/control-2.sock");
        assert_eq!(
            discover_eligible(vec![eligible.clone(), stale], Instant::now(), |path, _| {
                if path == eligible {
                    Ok(capability_response(true))
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "stale socket",
                    ))
                }
            },),
            Ok(eligible)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn one_eligible_plus_clean_capability_false_selects_the_eligible_one() {
        let eligible = PathBuf::from("/runtime/control-1.sock");
        let disabled = PathBuf::from("/runtime/control-2.sock");
        assert_eq!(
            discover_eligible(
                vec![eligible.clone(), disabled],
                Instant::now(),
                |path, _| Ok(capability_response(path == eligible)),
            ),
            Ok(eligible)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn inconsistent_capabilities_make_the_live_endpoint_unclassified() {
        let eligible = PathBuf::from("/runtime/control-1.sock");
        let inconsistent = PathBuf::from("/runtime/control-2.sock");
        assert_eq!(
            discover_eligible(
                vec![eligible.clone(), inconsistent.clone()],
                Instant::now(),
                |path, _| {
                    Ok(if path == eligible {
                        capability_response(true)
                    } else {
                        Response {
                            request_id: 1,
                            reply: Reply::Capabilities {
                                structural_control: false,
                                quick_terminal_toggle: true,
                            },
                        }
                    })
                },
            ),
            Err(DiscoveryFailure::Unclassified(vec![inconsistent]))
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn one_eligible_plus_clean_protocol_incompatibility_selects_the_eligible_one() {
        let eligible = PathBuf::from("/runtime/control-1.sock");
        let incompatible = PathBuf::from("/runtime/control-2.sock");
        assert_eq!(
            discover_eligible(
                vec![eligible.clone(), incompatible],
                Instant::now(),
                |path, _| {
                    if path == eligible {
                        Ok(capability_response(true))
                    } else {
                        Ok(Response {
                            request_id: 0,
                            reply: Reply::Error(ErrorCode::VersionMismatch),
                        })
                    }
                },
            ),
            Ok(eligible)
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn zero_candidate_and_multiple_candidate_messages_are_actionable() {
        let (none, success) = format_discovery_failure(DiscoveryFailure::NoRuntimeOrSockets, false);
        assert!(!success);
        assert!(none.contains("no running OdyTTY has automation_endpoint = on"));

        let first = PathBuf::from("/runtime/control-1.sock");
        let second = PathBuf::from("/runtime/control-2.sock");
        let (multiple, success) = format_discovery_failure(
            DiscoveryFailure::Multiple(vec![first.clone(), second.clone()]),
            true,
        );
        assert!(!success);
        assert!(multiple.contains("use --endpoint PATH"));
        assert!(multiple.contains(first.to_str().unwrap()));
        assert!(multiple.contains(second.to_str().unwrap()));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn enumeration_uncertainty_names_known_paths_in_human_and_json_output() {
        let path = PathBuf::from("/runtime/odytty/control-9.sock");
        let failure = || DiscoveryFailure::EnumerationUncertain(vec![path.clone()]);
        let message = "OdyTTY endpoint directory scan could not classify \"/runtime/odytty/control-9.sock\"; no toggle was sent; use --endpoint PATH";

        assert_eq!(
            format_discovery_failure(failure(), false),
            (format!("error unavailable: {message}\n"), false)
        );
        assert_eq!(
            format_discovery_failure(failure(), true),
            (
                format!(
                    "{{\"request_id\":1,\"error\":\"unavailable\",\"message\":\"{}\"}}\n",
                    json_escape(message)
                ),
                false,
            )
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn incomplete_discovery_budget_refuses_selection() {
        let unresolved = PathBuf::from("/runtime/control-1.sock");
        assert_eq!(
            discover_eligible(
                vec![unresolved.clone()],
                Instant::now() - DISCOVERY_BUDGET,
                |_, _| Ok(capability_response(true)),
            ),
            Err(DiscoveryFailure::Unclassified(vec![unresolved]))
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn endpoint_replaced_after_probe_is_one_unknown_outcome_without_retry() {
        let endpoint = PathBuf::from("/runtime/control-7.sock");
        let selected = discover_eligible(vec![endpoint.clone()], Instant::now(), |_, _| {
            Ok(capability_response(true))
        })
        .expect("probe selects endpoint");
        assert_eq!(selected, endpoint);
        let request = Request {
            version: VERSION,
            request_id: 9,
            action: Action::QuickTerminalToggle,
        };
        let calls = std::cell::Cell::new(0);
        let response = mutation_response(&request, || {
            calls.set(calls.get() + 1);
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "endpoint replaced",
            ))
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(response.reply, Reply::Error(ErrorCode::OutcomeUnknown));
    }

    #[cfg(windows)]
    #[test]
    fn windows_bare_toggle_refuses_with_explicit_pipe_guidance() {
        let command = parse(&args(&["control", "quick-terminal", "toggle"]))
            .expect("valid command")
            .expect("control command");
        let (output, success) = run(command);
        assert!(!success);
        assert_eq!(
            output,
            "error unavailable: Windows requires --endpoint \\\\.\\pipe\\odytty-control-<pid>\n"
        );
    }

    #[test]
    fn accepted_output_is_stable_in_human_and_json_formats() {
        let response = Response {
            request_id: 7,
            reply: Reply::Accepted,
        };
        assert_eq!(format_response(&response, false), "accepted\n");
        assert_eq!(
            format_response(&response, true),
            "{\"request_id\":7,\"accepted\":true}\n"
        );
    }

    #[test]
    fn object_ids_round_trip_and_refuse_malformed_input() {
        assert_eq!(format_id(&parse_id(WINDOW).unwrap()), WINDOW);
        for id in [
            "é0000000000000000000000000000000:window:1",
            "00000000000000000000000000000000:window:+1",
            "00000000000000000000000000000000:window:18446744073709551616",
            "00000000000000000000000000000000:window:1:extra",
        ] {
            assert!(parse_id(id).is_err());
        }
    }

    #[test]
    fn json_errors_are_stable_and_machine_readable() {
        let response = Response {
            request_id: 5,
            reply: Reply::Error(ErrorCode::OutcomeUnknown),
        };
        assert_eq!(
            format_response(&response, true),
            "{\"request_id\":5,\"error\":\"outcome_unknown\"}\n"
        );
    }

    #[test]
    fn cli_preserves_leading_dash_and_metacharacter_names_as_literals() {
        for name in ["-rf", "--help", "$(printf x)", "`id`", "a;b|c&&d"] {
            let Some(Command::Request { request, .. }) = parse(&args(&[
                "control",
                "--endpoint",
                "/example/control",
                "rename",
                WINDOW,
                name,
            ]))
            .unwrap() else {
                panic!("expected Request for {name:?}");
            };
            assert_eq!(
                request.action,
                Action::Rename {
                    target: parse_id(WINDOW).unwrap(),
                    name: name.into(),
                }
            );
        }
    }

    #[test]
    fn json_output_never_reflects_untrusted_name_or_path_bytes() {
        let hostile = "inject\"; DROP TABLE-- /tmp/secret";
        let Some(Command::Request { request, json, .. }) = parse(&args(&[
            "control",
            "--endpoint",
            "/example/control",
            "--json",
            "open-profile",
            WINDOW,
            hostile,
        ]))
        .unwrap() else {
            panic!("expected Request");
        };
        assert!(json);
        let response = Response {
            request_id: request.request_id,
            reply: Reply::Error(ErrorCode::PermissionDenied),
        };
        let rendered = format_response(&response, true);
        assert_eq!(
            rendered,
            "{\"request_id\":1,\"error\":\"permission_denied\"}\n"
        );
        assert!(
            !rendered.contains(hostile)
                && !rendered.contains("/tmp/secret")
                && !rendered.contains("DROP"),
            "JSON must not reflect untrusted name/path bytes: {rendered}"
        );
    }

    #[test]
    fn outcome_unknown_is_nonzero_exit_and_is_not_retried_by_run() {
        // No live endpoint: a structural mutation maps to OutcomeUnknown and
        // success=false. run() performs a single attempt (no retry loop).
        let Some(command) = parse(&args(&[
            "control",
            "--endpoint",
            "/example/missing-control.sock",
            "focus",
            WINDOW,
        ]))
        .unwrap() else {
            panic!("expected Request");
        };
        let (output, success) = run(command);
        assert!(
            !success,
            "OutcomeUnknown / transport failure must be nonzero"
        );
        assert!(
            output.contains("outcome_unknown") || output.contains("unavailable"),
            "output={output}"
        );
        assert_eq!(
            output.matches("outcome_unknown").count() + output.matches("unavailable").count(),
            1,
            "run must not retry: output={output}"
        );
    }
}
