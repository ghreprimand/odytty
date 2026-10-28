// SPDX-License-Identifier: GPL-3.0-only
//! Windows default-route transport and isolated package failures. Native tests
//! run on Windows CI; Linux/macOS do not claim Windows execution.
#![cfg(windows)]
#[path = "support/conpty_cleanup.rs"]
mod conpty_cleanup;
#[path = "support/conpty_replies.rs"]
mod conpty_replies;
#[path = "../src/test_dirs/allocation.rs"]
pub mod scratch_dirs;
mod support;

use odytty::core::{Dimensions, Terminal};
use odytty::pty::PtySession;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
const FIXED: &[u8; 86] = b"<ODYTTY-SIXEL-BEFORE-V1>\x1b[2J\x1b[2;11H\x1bPq#0;2;100;0;0#0~~~~-~~~~\x1b\\<ODYTTY-SIXEL-AFTER-V1>";
const DCS: &[u8] = b"\x1bPq#0;2;100;0;0#0~~~~-~~~~\x1b\\";
const RUNTIME: &str = "runtime/conpty/1.25.260930003";
fn dimensions() -> Dimensions {
    Dimensions {
        rows: 24,
        columns: 80,
    }
}
fn current_dir() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}

// Only an owned helper executes PTY work. Its job contains the helper and all
// newly created hosts/clients; hard deadline termination closes that job too.
fn contain_helper() {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows::Win32::System::JobObjects::*;
    use windows::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: newly owned job, exact information structure, own process only.
    unsafe {
        let job = CreateJobObjectW(None, None).unwrap();
        let handle = OwnedHandle::from_raw_handle(job.0);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
        .unwrap();
        AssignProcessToJobObject(job, GetCurrentProcess()).unwrap();
        // Process-lifetime owner: closing here would kill the helper itself.
        std::mem::forget(handle);
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(45));
        eprintln!("conpty_product_helper=deadline");
        std::process::exit(124);
    });
}
fn run_owned(exe: &Path, mode: &str, cwd: &Path) {
    let child_env = scratch_dirs::ChildEnv::new();
    let mut child = Command::new(exe)
        .args(["--exact", "product_child", "--nocapture"])
        .envs(child_env.vars())
        .env("ODYTTY_CONPTY_PRODUCT_CHILD", mode)
        .current_dir(cwd)
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.by_ref().take(65537).read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(55);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned helper exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = out.join().unwrap();
    let err = err.join().unwrap();
    assert!(
        out.len() <= 65536 && err.len() <= 65536,
        "helper output bound"
    );
    assert!(
        status.success(),
        "helper {mode}: stdout={} stderr={}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    assert!(String::from_utf8_lossy(&out).contains("conpty_product_helper=pass"));
    println!("conpty_product_case={mode};result=pass");
}
fn collect(session: &mut PtySession) -> (Vec<u8>, Terminal, usize) {
    let mut reader = session.try_clone_reader().unwrap();
    let mut writer = session.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut terminal = Terminal::new(80, 24);
        let mut raw = Vec::new();
        let mut replies = conpty_replies::Replies::default();
        let mut read_failure = None;
        let mut buffer = [0; 2048];
        let eof = loop {
            let n = match reader.read(&mut buffer) {
                Ok(n) => n,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    read_failure = Some(error.kind());
                    break false;
                }
            };
            if n == 0 {
                break true;
            }
            assert!(raw.len() + n <= 65536, "transport output cap");
            // Capture raw evidence before the production parser sees it.
            raw.extend_from_slice(&buffer[..n]);
            terminal.advance(&buffer[..n]);
            if replies
                .submit(&mut writer, &terminal.take_host_output())
                .is_err()
            {
                break false;
            }
        };
        let _ = tx.send((raw, terminal, replies, eof, read_failure));
    });
    let (raw, terminal, replies, eof, read_failure) = rx
        .recv_timeout(Duration::from_secs(35))
        .expect("transport observation deadline");
    thread.join().expect("transport reader thread");
    assert!(
        replies.complete(),
        "nonempty host reply incomplete: {replies:?}; eof={eof}; read_failure={read_failure:?}"
    );
    assert!(
        read_failure.is_none() && eof,
        "transport did not reach EOF: {read_failure:?}; replies={replies:?}"
    );
    assert!(session.wait().unwrap().success());
    println!(
        "conpty_replies generated_bytes={} written_bytes={} completed_replies={} complete=true eof=true",
        replies.generated_bytes, replies.written_bytes, replies.completed_replies
    );
    (raw, terminal, replies.written_bytes)
}
fn default_transport() {
    let scratch = support::TempDir::new("conpty-fixed-", false);
    let attestation = scratch.path().join("producer.json");
    let producer = current_dir().join("fixed-producer.exe");
    // Public default route. No provider override or diagnostic API exists.
    let mut session = PtySession::spawn_exec(
        dimensions(),
        producer.into_os_string(),
        vec!["--attestation".into(), attestation.clone().into_os_string()],
        None,
    )
    .unwrap();
    let (raw, terminal, replies) = collect(&mut session);
    assert!(
        raw.windows(DCS.len()).any(|chunk| chunk == DCS),
        "exact fixed DCS before Terminal"
    );
    for marker in [
        b"<ODYTTY-SIXEL-BEFORE-V1>".as_slice(),
        b"<ODYTTY-SIXEL-AFTER-V1>".as_slice(),
    ] {
        assert!(raw.windows(marker.len()).any(|chunk| chunk == marker));
    }
    let mut expected = Terminal::new(80, 24);
    expected.advance(FIXED);
    assert_eq!(terminal.screen().cursor(), expected.screen().cursor());
    let placements = terminal.graphics().placements();
    assert_eq!(placements.len(), 1);
    let image = terminal
        .graphics()
        .store()
        .get(placements[0].image_id)
        .unwrap();
    assert_eq!((image.width, image.height), (4, 12));
    assert_eq!(image.rgba.len(), 192);
    assert!(
        image
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel == [255, 0, 0, 255])
    );
    let reference = &expected.graphics().placements()[0];
    assert_eq!(placements[0].anchor, reference.anchor);
    assert_eq!(placements[0].protocol, reference.protocol);
    assert_eq!(placements[0].display_columns, reference.display_columns);
    assert_eq!(placements[0].display_rows, reference.display_rows);
    let bytes = std::fs::read(attestation).unwrap();
    assert!(bytes.len() <= 512);
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "{\"schema\":\"sixel-producer-v2\",\"route\":\"native_writefile\",\"known_byte_count\":86,\"known_sha256\":\"13cb1d2002030915947d9f963640badf36e1d443926e8a084179ec75b97e011a\",\"console_mode\":\"vt_enabled\",\"os_write\":\"complete_known_submission\"}\n"
    );
    println!(
        "conpty_default_transport=exact_dcs;core_image=true;core_cursor=true;eof=true;reply_bytes={replies}"
    );
}
fn host_identity_and_shell() {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    let script = r#"
[Console]::OutputEncoding=[Console]::InputEncoding=[Text.UTF8Encoding]::new($false);
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class OwnConsole { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p); }';
[uint32]$owner=0; $window=[OwnConsole]::GetConsoleWindow(); [void][OwnConsole]::GetWindowThreadProcessId($window,[ref]$owner);
[Console]::WriteLine(('OWN_HOST={0};' -f $owner));
[Console]::WriteLine(('CWD_MATCH='+((Get-Location).ProviderPath -eq $env:ODYTTY_NATIVE_EXPECTED_CWD)));
[Console]::WriteLine(('ENV_VALUE='+$env:ODYTTY_NATIVE_VALUE));
[Console]::WriteLine('READY_INPUT');
$line=[Console]::ReadLine(); [Console]::WriteLine(('ECHO_INPUT={0};' -f $line));
Start-Sleep -Seconds 3
"#;
    let program = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let scratch = support::TempDir::new("conpty-shell-", false);
    let cwd = scratch.path().join("cwd space \u{03b1}");
    std::fs::create_dir(&cwd).unwrap();
    let mut command = odytty::pty::CommandBuilder::new(program.into_os_string());
    for arg in ["-NoProfile", "-NonInteractive", "-Command", script] {
        command.arg(arg);
    }
    command.current_dir(cwd.clone());
    command.env("ODYTTY_NATIVE_EXPECTED_CWD", cwd.into_os_string());
    command.env("ODYTTY_NATIVE_VALUE", "value \u{03b1} \u{4e2d} 'quoted'");
    let mut session = PtySession::spawn_command(dimensions(), command).unwrap();
    let mut reader = session.try_clone_reader().unwrap();
    let mut writer = session.take_writer().unwrap();
    let (tx, rx) = mpsc::channel::<Result<u32, String>>();
    let thread = std::thread::spawn(move || -> Result<conpty_replies::Replies, String> {
        let mut raw = Vec::new();
        let mut terminal = Terminal::new(80, 24);
        let mut sent = false;
        let mut replies = conpty_replies::Replies::default();
        loop {
            let mut buffer = [0; 2048];
            let n = match reader.read(&mut buffer) {
                Ok(n) => n,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let reason =
                        format!("host_read_failed={:?}; replies={replies:?}", error.kind());
                    let _ = tx.send(Err(reason.clone()));
                    return Err(reason);
                }
            };
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..n]);
            assert!(raw.len() <= 32768);
            terminal.advance(&buffer[..n]);
            if replies
                .submit(&mut writer, &terminal.take_host_output())
                .is_err()
            {
                let reason = format!("nonempty_host_reply_failed: {replies:?}");
                let _ = tx.send(Err(reason.clone()));
                return Err(reason);
            }
            if !sent && String::from_utf8_lossy(&raw).contains("READY_INPUT") {
                let text = String::from_utf8_lossy(&raw);
                let tail = text.split("OWN_HOST=").nth(1).expect("host marker");
                let pid: u32 = tail.split(';').next().unwrap().parse().unwrap();
                assert!(pid > 0);
                tx.send(Ok(pid)).expect("host observation receiver");
                if let Err(error) = writer.write_all("input \u{03b1} \u{4e2d}\r".as_bytes()) {
                    return Err(format!(
                        "unicode_input_write_failed={:?}; replies={replies:?}",
                        error.kind()
                    ));
                }
                sent = true;
            }
        }
        if !sent {
            let reason = format!("own_host_marker_missing; replies={replies:?}");
            let _ = tx.send(Err(reason.clone()));
            return Err(reason);
        }
        assert!(String::from_utf8_lossy(&raw).contains("ECHO_INPUT=input \u{03b1} \u{4e2d};"));
        assert!(String::from_utf8_lossy(&raw).contains("CWD_MATCH=True"));
        assert!(
            String::from_utf8_lossy(&raw).contains("ENV_VALUE=value \u{03b1} \u{4e2d} 'quoted'")
        );
        Ok(replies)
    });
    let pid = rx
        .recv_timeout(Duration::from_secs(25))
        .expect("own attached console observation deadline")
        .unwrap_or_else(|reason| panic!("{reason}"));
    // Query only the PID supplied by our owned child's attached console window.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.unwrap();
    let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
    let mut wide = [0; 1024];
    let mut len = wide.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(handle.as_raw_handle()),
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(wide.as_mut_ptr()),
            &mut len,
        )
    }
    .unwrap();
    let image = PathBuf::from(OsString::from_wide(&wide[..len as usize]))
        .canonicalize()
        .unwrap();
    let base = current_dir().join(RUNTIME);
    let expected = [
        base.join("x64/OpenConsole.exe").canonicalize().unwrap(),
        base.join("arm64/OpenConsole.exe").canonicalize().unwrap(),
    ];
    assert!(
        expected.contains(&image),
        "own console must use the pinned companion"
    );
    session
        .resize(Dimensions {
            rows: 25,
            columns: 81,
        })
        .unwrap();
    let replies = thread
        .join()
        .expect("host reader thread")
        .unwrap_or_else(|reason| panic!("{reason}"));
    assert!(
        replies.complete(),
        "host replies incomplete at EOF: {replies:?}"
    );
    println!(
        "conpty_host_replies generated_bytes={} written_bytes={} completed_replies={} complete=true eof=true",
        replies.generated_bytes, replies.written_bytes, replies.completed_replies
    );
    assert!(session.wait().unwrap().success());
    println!("conpty_owned_host=verified;unicode_input=true;resize=true;eof=true");
}
#[test]
fn product_child() {
    let Some(mode) = std::env::var_os("ODYTTY_CONPTY_PRODUCT_CHILD") else {
        return;
    };
    contain_helper();
    match mode.to_str().unwrap() {
        "transport" => default_transport(),
        "late-shadow" => {
            default_transport();
            let shadow = current_dir().join(RUNTIME).join("OpenConsole.exe");
            if std::fs::write(&shadow, b"late shadow").is_ok() {
                let result = PtySession::spawn_shell_command(dimensions(), "exit 0");
                assert!(
                    result.is_err(),
                    "cached provider accepted a new shadow host"
                );
                assert!(
                    result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("reinstall the complete Windows package folder")
                );
                std::fs::remove_file(shadow).unwrap();
            }
        }
        "host" => host_identity_and_shell(),
        "missing" => {
            let result = PtySession::spawn_shell_command(dimensions(), "exit 0");
            let error = match result {
                Ok(_) => panic!("incomplete runtime accepted"),
                Err(e) => e,
            };
            assert!(
                error
                    .to_string()
                    .contains("reinstall the complete Windows package folder")
            );
        }
        _ => panic!("unknown owned helper mode"),
    }
    println!("conpty_product_helper=pass");
}
#[test]
fn default_public_spawn_delivers_fixed_sixel_and_core_state() {
    run_owned(
        &std::env::current_exe().unwrap(),
        "transport",
        &current_dir(),
    );
}
#[test]
fn default_public_spawn_uses_owned_bundled_host_and_unicode_input() {
    run_owned(&std::env::current_exe().unwrap(), "host", &current_dir());
}
#[test]
fn fresh_package_unicode_spaces_hostile_cwd_and_missing_asset_failures() {
    let scratch = support::TempDir::new("conpty-package-", false);
    let package = scratch.path().join("package space \u{03b1}");
    std::fs::create_dir(&package).unwrap();
    let exe = package.join("product-test.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
    let hostile = scratch.path().join("cwd");
    std::fs::create_dir(&hostile).unwrap();
    for name in [
        "conpty.dll",
        "OpenConsole.exe",
        "icu.dll",
        "D3DCOMPILER_47.dll",
    ] {
        std::fs::write(hostile.join(name), b"must never load").unwrap();
    }
    run_owned(&exe, "missing", &hostile);
    for arch in ["x64", "arm64"] {
        std::fs::create_dir_all(package.join(RUNTIME).join(arch)).unwrap();
    }
    for member in ["conpty.dll", "x64/OpenConsole.exe", "arm64/OpenConsole.exe"] {
        std::fs::copy(
            current_dir().join(RUNTIME).join(member),
            package.join(RUNTIME).join(member),
        )
        .unwrap();
    }
    std::fs::copy(
        current_dir().join("fixed-producer.exe"),
        package.join("fixed-producer.exe"),
    )
    .unwrap();
    run_owned(&exe, "transport", &hostile);
    run_owned(&exe, "host", &hostile);
    run_owned(&exe, "late-shadow", &hostile);
    for member in ["conpty.dll", "x64/OpenConsole.exe", "arm64/OpenConsole.exe"] {
        let path = package.join(RUNTIME).join(member);
        let bytes = std::fs::read(&path).unwrap();
        conpty_cleanup::retry(|| std::fs::remove_file(&path))
            .expect("remove native member after helper exit");
        run_owned(&exe, "missing", &hostile);
        let mut changed = bytes.clone();
        changed[64] ^= 1;
        std::fs::write(&path, changed).unwrap();
        run_owned(&exe, "missing", &hostile);
        std::fs::write(&path, bytes).unwrap();
    }
    let shadow = package.join(RUNTIME).join("OpenConsole.exe");
    std::fs::write(&shadow, b"shadow").unwrap();
    run_owned(&exe, "missing", &hostile);
    conpty_cleanup::retry(|| std::fs::remove_file(&shadow))
        .expect("remove shadow after helper exit");
    let host_dir = package.join(RUNTIME).join("arm64");
    let outside = scratch.path().join("outside");
    conpty_cleanup::retry(|| std::fs::rename(&host_dir, &outside))
        .expect("move host directory after helper exit");
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let child_env = scratch_dirs::ChildEnv::new();
    let mut junction_child=Command::new(powershell).args(["-NoProfile","-NonInteractive","-Command",
        "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:ODYTTY_OWN_LINK -Target $env:ODYTTY_OWN_TARGET | Out-Null"])
        .envs(child_env.vars()).env("ODYTTY_OWN_LINK",&host_dir).env("ODYTTY_OWN_TARGET",&outside)
        .creation_flags(0x0800_0000).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = junction_child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = junction_child.kill();
            let _ = junction_child.wait();
            panic!("owned junction deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "owned junction fixture");
    run_owned(&exe, "missing", &hostile);
    conpty_cleanup::retry(|| std::fs::remove_dir(&host_dir))
        .expect("remove junction after helper exit");
    conpty_cleanup::retry(|| std::fs::rename(&outside, &host_dir))
        .expect("restore host directory after helper exit");
    // An offline complete-folder replacement after all processes have exited.
    let upgraded = scratch.path().join("replacement");
    conpty_cleanup::retry(|| std::fs::rename(&package, &upgraded))
        .expect("replace package after helper exit");
    run_owned(&upgraded.join("product-test.exe"), "transport", &hostile);
    conpty_cleanup::retry(|| std::fs::remove_dir_all(scratch.path()))
        .expect("clean package after all owned helpers exited");
}
