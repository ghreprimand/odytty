// SPDX-License-Identifier: GPL-3.0-only
//! Typed control CLI. Explicit endpoint, no shell parsing or automatic retries.

use std::ffi::OsString;
use std::path::PathBuf;

use super::protocol::{
    self, Action, ErrorCode, ObjectId, ObjectKind, Reply, Request, Response, SplitDirection,
    VERSION,
};

pub const USAGE: &str = "Usage: odytty control --endpoint PATH [--json] COMMAND [ARGS]\n\
Commands: capabilities | list | status ID | focus ID | open-profile WINDOW NAME |\n\
          create-tab WINDOW | create-workspace WINDOW NAME |\n\
          split PANE columns|rows | rename ID NAME\n\
IDs: 32-hex-instance:window|workspace|tab|pane:decimal-serial\n\
The endpoint must be explicitly enabled by its owner. No terminal input or content reads.\n\
Unix requires an existing owner-private directory. Windows transport is unavailable.\n\
A lost mutation reply has an unknown outcome; do not retry automatically.\n";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Request {
        endpoint: PathBuf,
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
    let endpoint = endpoint
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or(ErrorCode::InvalidRequest)?;
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
        _ => return Err(ErrorCode::InvalidRequest),
    };
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
            Reply::Capabilities { structural_control } => format!(
                "\"capabilities\":{{\"protocol_version\":{VERSION},\"structural_control\":{structural_control},\"terminal_input\":false,\"terminal_content\":false}}"
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
            Reply::Error(error) => format!("\"error\":\"{error}\""),
        };
        format!("{{\"request_id\":{request_id},{body}}}\n")
    } else {
        match &response.reply {
            Reply::Capabilities { structural_control } => format!(
                "protocol_version={VERSION} structural_control={structural_control} terminal_input=false terminal_content=false\n"
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
    let response = super::unix::request(&endpoint, &request).unwrap_or_else(|_| Response {
        request_id: request.request_id,
        reply: Reply::Error(if request.action.is_read_only() {
            ErrorCode::Unavailable
        } else {
            ErrorCode::OutcomeUnknown
        }),
    });
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
}
