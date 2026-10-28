// SPDX-License-Identifier: GPL-3.0-only
//! Standalone fixed-input console producer. Compile in CI, never on the recipient.
use std::io::Write;

#[cfg(any(windows, test))]
const TRANSACTION: &[u8; 86] = b"<ODYTTY-SIXEL-BEFORE-V1>\x1b[2J\x1b[2;11H\x1bPq#0;2;100;0;0#0~~~~-~~~~\x1b\\<ODYTTY-SIXEL-AFTER-V1>";
const DIGEST: &str = "13cb1d2002030915947d9f963640badf36e1d443926e8a084179ec75b97e011a";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Unavailable,
    Enabled,
    Disabled,
}
impl Mode {
    fn observed(result: Option<u32>) -> Self {
        match result {
            Some(bits) if bits & 4 != 0 => Self::Enabled,
            Some(_) => Self::Disabled,
            None => Self::Unavailable,
        }
    }
    fn category(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable_unknown",
            Self::Enabled => "vt_enabled",
            Self::Disabled => "vt_disabled",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Submission {
    Unavailable,
    Failed,
    Incomplete,
    Complete,
}
impl Submission {
    fn observed(result: Option<(bool, u32)>) -> Self {
        match result {
            Some((true, 86)) => Self::Complete,
            Some((true, _)) => Self::Incomplete,
            Some((false, _)) => Self::Failed,
            None => Self::Unavailable,
        }
    }
    fn category(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable_unknown",
            Self::Failed => "os_write_failed_unknown",
            Self::Incomplete => "os_write_incomplete_unknown",
            Self::Complete => "complete_known_submission",
        }
    }
}
fn record(mode: Mode, submission: Submission) -> String {
    format!(
        "{{\"schema\":\"sixel-producer-v2\",\"route\":\"native_writefile\",\"known_byte_count\":86,\"known_sha256\":\"{DIGEST}\",\"console_mode\":\"{}\",\"os_write\":\"{}\"}}\n",
        mode.category(),
        submission.category()
    )
}

#[cfg(windows)]
fn submit() -> (Mode, Submission) {
    use std::ffi::c_void;
    // These are existing system APIs. No mode, module or handle ownership changes.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(kind: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn WriteFile(
            handle: *mut c_void,
            data: *const u8,
            len: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }
    // SAFETY: querying our own standard handle does not transfer ownership.
    let handle = unsafe { GetStdHandle((-11i32) as u32) };
    if handle.is_null() || handle as isize == -1 {
        return (Mode::Unavailable, Submission::Unavailable);
    }
    let mut bits = 0;
    // SAFETY: bits is a valid DWORD out pointer for the borrowed stdout handle.
    let queried = unsafe { GetConsoleMode(handle, &mut bits) } != 0;
    let mode = Mode::observed(queried.then_some(bits));
    let mut written = 0;
    // SAFETY: the immutable fixed buffer lives through this synchronous call;
    // the count pointer is valid and a null OVERLAPPED requests synchronous I/O.
    // Exactly one call, no retry. Owned launcher teardown bounds a blocked call.
    let success = unsafe {
        WriteFile(
            handle,
            TRANSACTION.as_ptr(),
            86,
            &mut written,
            std::ptr::null_mut(),
        )
    } != 0;
    (mode, Submission::observed(Some((success, written))))
}
#[cfg(not(windows))]
fn submit() -> (Mode, Submission) {
    (Mode::observed(None), Submission::observed(None))
}

fn produce(path: &std::path::Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("attestation unavailable"));
    }
    std::thread::sleep(std::time::Duration::from_secs(1));
    let (mode, submission) = submit();
    let bytes = record(mode, submission);
    file.write_all(bytes.as_bytes())?;
    file.flush()?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    Ok(())
}
fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os().skip(1);
    let flag = args.next();
    let path = args.next();
    if flag.as_deref() != Some(std::ffi::OsStr::new("--attestation"))
        || path.is_none()
        || args.next().is_some()
    {
        eprintln!("sixel_producer_v2 arguments_unknown");
        return std::process::ExitCode::from(2);
    }
    if produce(std::path::Path::new(&path.unwrap())).is_err() {
        eprintln!("sixel_producer_v2 attestation_or_setup_unknown");
        return std::process::ExitCode::from(2);
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_success_and_full_count_attests_submission() {
        assert_eq!(Submission::observed(Some((true, 86))), Submission::Complete);
        for count in [0, 1, 85, 87, u32::MAX] {
            assert_eq!(
                Submission::observed(Some((true, count))),
                Submission::Incomplete
            );
        }
        for count in [0, 86, u32::MAX] {
            assert_eq!(
                Submission::observed(Some((false, count))),
                Submission::Failed
            );
        }
        assert_eq!(Submission::observed(None), Submission::Unavailable);
    }
    #[test]
    fn mode_failure_is_unknown_not_disabled_or_not_console() {
        assert_eq!(Mode::observed(None), Mode::Unavailable);
        assert_eq!(Mode::observed(Some(0)), Mode::Disabled);
        assert_eq!(Mode::observed(Some(4)), Mode::Enabled);
        assert_eq!(Mode::observed(Some(u32::MAX)), Mode::Enabled);
    }
    #[test]
    fn records_are_fixed_bounded_and_never_contain_payload_or_counts() {
        assert_eq!(TRANSACTION.len(), 86);
        for mode in [Mode::Unavailable, Mode::Enabled, Mode::Disabled] {
            for submission in [
                Submission::Unavailable,
                Submission::Failed,
                Submission::Incomplete,
                Submission::Complete,
            ] {
                let output = record(mode, submission);
                assert!(output.len() < 512);
                assert!(!output.contains("<ODYTTY"));
                assert!(!output.contains("written_count"));
                assert!(!output.contains("not_console"));
            }
        }
    }
    #[test]
    fn existing_attestation_is_preserved_without_submission() {
        // No produce call or OS I/O: verify the exclusive file-open boundary.
        let path =
            std::env::temp_dir().join(format!("odytty-producer-contract-{}", std::process::id()));
        let mut first = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Cleanup(path.clone());
        first.write_all(b"existing").unwrap();
        assert!(produce(&path).is_err());
        drop(first);
        assert_eq!(std::fs::read(&path).unwrap(), b"existing");
    }
    #[cfg(windows)]
    #[test]
    fn owned_redirected_pipe_receives_fixed_submission_and_attestation() {
        use std::io::Read;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        let exe = std::env::var_os("ODYTTY_SIXEL_PRODUCER_TEST_EXE")
            .expect("producer test executable is required");
        let dir = std::env::temp_dir().join(format!("odytty-producer-pipe-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("producer.json");
        let mut child = Command::new(exe)
            .arg("--attestation")
            .arg(&path)
            .creation_flags(0x08000000)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.take(87).read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let bytes = reader.join().unwrap();
        assert!(status.is_some_and(|status| status.success()));
        assert_eq!(bytes, TRANSACTION);
        assert!(std::fs::metadata(&path).unwrap().len() < 512);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            record(Mode::Unavailable, Submission::Complete)
        );
    }
}
