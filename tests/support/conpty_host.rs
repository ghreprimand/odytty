// SPDX-License-Identifier: GPL-3.0-only
//! Exact running-file identity inside the helper's private job, with finite attribution.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    volume: u64,
    id: [u8; 16],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    HelperSelf,
    Shell,
    PinnedHost,
    SystemConhost,
    OtherOpenconsole,
    OtherConsole,
    Other,
    OtherUnreadable,
    Unknown,
}
#[derive(Default, Debug)]
struct Counts {
    members: usize,
    helper_self: usize,
    shell: usize,
    pinned_host: usize,
    system_conhost: usize,
    other_openconsole: usize,
    other_console: usize,
    other: usize,
    other_unreadable: usize,
}
impl Counts {
    fn add(&mut self, class: Class) {
        match class {
            Class::HelperSelf => self.helper_self += 1,
            Class::Shell => self.shell += 1,
            Class::PinnedHost => self.pinned_host += 1,
            Class::SystemConhost => self.system_conhost += 1,
            Class::OtherOpenconsole => self.other_openconsole += 1,
            Class::OtherConsole => self.other_console += 1,
            Class::Other => self.other += 1,
            Class::OtherUnreadable => self.other_unreadable += 1,
            Class::Unknown => {}
        }
    }
    fn selected(&self) -> bool {
        self.helper_self == 1
            && self.shell == 1
            && self.pinned_host == 1
            && self.system_conhost == 0
            && self.other_openconsole == 0
            && self.other_console == 0
    }
}
fn classify(
    id: Identity,
    name: &str,
    helper: Identity,
    shell: Identity,
    system: Identity,
    pinned: [Identity; 2],
) -> Class {
    if pinned.contains(&id) {
        Class::PinnedHost
    } else if id == system {
        Class::SystemConhost
    } else if id == helper {
        Class::HelperSelf
    } else if id == shell {
        Class::Shell
    } else {
        named_class(name)
    }
}
fn named_class(name: &str) -> Class {
    if name.eq_ignore_ascii_case("OpenConsole.exe") {
        Class::OtherOpenconsole
    } else if name.eq_ignore_ascii_case("conhost.exe") {
        Class::OtherConsole
    } else {
        Class::Other
    }
}
fn bounded_count(assigned: u32, count: u32) -> Result<usize, &'static str> {
    if assigned != count || count == 0 || count > 64 {
        Err("own_job_list_incomplete_or_over_capacity")
    } else {
        Ok(count as usize)
    }
}

fn error_label(code: u32) -> &'static str {
    match code {
        2 | 3 | 6 => "vanished",
        87 => "retryable_or_unsupported",
        _ => "other",
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    use windows::Win32::System::JobObjects::{
        IsProcessInJob, JobObjectBasicProcessIdList, QueryInformationJobObject,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    #[derive(Debug)]
    struct Failure {
        step: &'static str,
        class: Class,
        retryable: bool,
        label: &'static str,
    }
    fn failure(step: &'static str, class: Class, code: u32) -> Failure {
        Failure {
            step,
            class,
            retryable: matches!(code, 2 | 3 | 6 | 87),
            label: error_label(code),
        }
    }
    fn identity(path: &Path, class: Class) -> Result<(std::fs::File, Identity), Failure> {
        let file = std::fs::File::open(path).map_err(|e| {
            failure(
                "image_file_open",
                class,
                e.raw_os_error().unwrap_or(0) as u32,
            )
        })?;
        let mut info = FILE_ID_INFO::default();
        // SAFETY: live owned file, exact structure and buffer size. FileIdInfo
        // retains the full 128-bit identity on ReFS as well as NTFS.
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileIdInfo,
                (&raw mut info).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        }
        .map_err(|e| failure("identity", class, e.code().0 as u32 & 0xffff))?;
        Ok((
            file,
            Identity {
                volume: info.VolumeSerialNumber,
                id: info.FileId.Identifier,
            },
        ))
    }
    // ABI: two DWORD counts followed by ULONG_PTR IDs. Fixed capacity, never a
    // system-wide snapshot. Nested child jobs are included by this API.
    #[repr(C)]
    struct List {
        assigned: u32,
        count: u32,
        ids: [usize; 64],
    }
    fn observe(
        job: HANDLE,
        helper: Identity,
        shell: Identity,
        system: Identity,
        pinned: [Identity; 2],
        counts: &mut Counts,
    ) -> Result<(), Failure> {
        let mut list = List {
            assigned: 0,
            count: 0,
            ids: [0; 64],
        };
        // SAFETY: explicit helper-owned job and aligned, fixed ABI buffer.
        unsafe {
            QueryInformationJobObject(
                Some(job),
                JobObjectBasicProcessIdList,
                (&raw mut list).cast(),
                std::mem::size_of_val(&list) as u32,
                None,
            )
        }
        .map_err(|_| failure("job_list", Class::Unknown, 0))?;
        let count = bounded_count(list.assigned, list.count)
            .map_err(|_| failure("job_capacity", Class::Unknown, 0))?;
        counts.members = count;
        for &pid in &list.ids[..count] {
            let pid = u32::try_from(pid)
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| failure("pid", Class::Unknown, 0))?;
            // SAFETY: only IDs supplied by our explicit private job. Recheck
            // membership on the opened handle to reject PID reuse outside it.
            let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
                .map_err(|e| failure("open", Class::Unknown, e.code().0 as u32 & 0xffff))?;
            let process = unsafe { OwnedHandle::from_raw_handle(raw.0) };
            let mut member = windows::core::BOOL::default();
            unsafe { IsProcessInJob(HANDLE(process.as_raw_handle()), Some(job), &raw mut member) }
                .map_err(|_| failure("membership", Class::Unknown, 0))?;
            if !member.as_bool() {
                return Err(failure("membership_changed", Class::Unknown, 0));
            }
            let mut wide = [0u16; 1024];
            let mut length = wide.len() as u32;
            unsafe {
                QueryFullProcessImageNameW(
                    HANDLE(process.as_raw_handle()),
                    PROCESS_NAME_WIN32,
                    windows::core::PWSTR(wide.as_mut_ptr()),
                    &raw mut length,
                )
            }
            .map_err(|e| failure("image_query", Class::Unknown, e.code().0 as u32 & 0xffff))?;
            if length == 0 || length as usize >= wide.len() {
                return Err(failure("image_capacity", Class::Unknown, 0));
            }
            let image = PathBuf::from(std::ffi::OsString::from_wide(&wide[..length as usize]));
            let name = image.file_name().unwrap_or_default().to_string_lossy();
            let class = named_class(&name);
            match identity(&image, class) {
                Ok((_file, id)) => counts.add(classify(id, &name, helper, shell, system, pinned)),
                Err(e) if !e.retryable && class == Class::Other => {
                    // This cannot count as the shell, helper or a pinned host.
                    println!(
                        "conpty_host_member=other_unreadable;step={};class={:?}",
                        e.step, e.class
                    );
                    counts.add(Class::OtherUnreadable);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    pub fn verify(job: HANDLE, base: &Path) -> Result<(), String> {
        let result = (|| {
            let system = PathBuf::from(
                std::env::var_os("SystemRoot")
                    .ok_or_else(|| failure("system_root", Class::Unknown, 0))?,
            )
            .join("System32");
            let expected = [
                identity(&base.join("x64/OpenConsole.exe"), Class::PinnedHost)?,
                identity(&base.join("arm64/OpenConsole.exe"), Class::PinnedHost)?,
            ];
            let helper = identity(
                &std::env::current_exe()
                    .map_err(|_| failure("helper_path", Class::HelperSelf, 0))?,
                Class::HelperSelf,
            )?;
            let shell = identity(
                &system.join("WindowsPowerShell/v1.0/powershell.exe"),
                Class::Shell,
            )?;
            let inbox = identity(&system.join("conhost.exe"), Class::SystemConhost)?;
            let start = std::time::Instant::now();
            for attempt in 1..=3 {
                let mut counts = Counts::default();
                let observation = observe(
                    job,
                    helper.1,
                    shell.1,
                    inbox.1,
                    [expected[0].1, expected[1].1],
                    &mut counts,
                );
                println!(
                    "conpty_host_identity=observed;attempt={attempt};counts={counts:?};error={observation:?}"
                );
                match observation {
                    Ok(()) if counts.selected() => return Ok(()),
                    Ok(()) => {
                        return Err(failure("own_job_pinned_host_mismatch", Class::Unknown, 0));
                    }
                    Err(e)
                        if e.retryable
                            && attempt < 3
                            && start.elapsed() < std::time::Duration::from_millis(750) =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(250))
                    }
                    Err(e) => return Err(e),
                }
            }
            unreachable!("bounded observation returns on last attempt")
        })();
        if let Err(e) = &result {
            println!(
                "conpty_host_identity=failed;step={};class={:?};error_label={}",
                e.step, e.class, e.label
            );
        }
        result.map_err(|e| format!("{};class={:?};error_label={}", e.step, e.class, e.label))
    }
}
#[cfg(windows)]
pub use native::verify;

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> Identity {
        Identity {
            volume: 1,
            id: [n; 16],
        }
    }
    fn classify_fixture(actual: Identity, name: &str) -> Class {
        classify(actual, name, id(1), id(2), id(3), [id(4), id(5)])
    }
    #[test]
    fn error_87_does_not_claim_process_vanished() {
        assert_eq!(error_label(87), "retryable_or_unsupported");
        assert_eq!(error_label(2), "vanished");
    }
    #[test]
    fn exact_identity_overrides_case_alias_but_same_basename_does_not() {
        assert_eq!(
            classify_fixture(id(4), "OPENCONSOLE.EXE"),
            Class::PinnedHost
        );
        assert_eq!(
            classify_fixture(id(9), "OpenConsole.exe"),
            Class::OtherOpenconsole
        );
        assert_eq!(classify_fixture(id(3), "CONHOST.EXE"), Class::SystemConhost);
        assert_eq!(classify_fixture(id(9), "conhost.exe"), Class::OtherConsole);
        assert_eq!(
            classify_fixture(Identity { volume: 2, ..id(4) }, "OpenConsole.exe"),
            Class::OtherOpenconsole
        );
        let mut differing_high_bits = id(4);
        differing_high_bits.id[15] = 9;
        assert_eq!(
            classify_fixture(differing_high_bits, "OpenConsole.exe"),
            Class::OtherOpenconsole
        );
    }
    #[test]
    fn incomplete_or_oversized_job_observations_fail_closed() {
        for (assigned, count) in [(2, 1), (65, 64), (65, 65), (0, 0)] {
            assert!(bounded_count(assigned, count).is_err());
        }
        assert_eq!(bounded_count(64, 64).unwrap(), 64);
    }
    #[test]
    fn all_members_are_counted_and_missing_duplicate_or_foreign_hosts_fail() {
        let classify = |ids: &[Identity]| {
            let mut c = Counts {
                members: ids.len(),
                ..Counts::default()
            };
            for id in ids {
                c.add(classify_fixture(*id, "OpenConsole.exe"));
            }
            c
        };
        for ids in [
            vec![id(1), id(2)],
            vec![id(1), id(2), id(4), id(5)],
            vec![id(1), id(2), id(3)],
            vec![id(1), id(2), id(4), id(9)],
        ] {
            let counts = classify(&ids);
            assert_eq!(counts.members, ids.len());
            assert!(!counts.selected());
        }
        let mut counts = classify(&[id(1), id(2), id(4)]);
        assert!(counts.selected());
        counts.add(Class::OtherUnreadable);
        counts.add(Class::Unknown);
        assert_eq!(counts.other_unreadable, 1);
        assert!(counts.selected());
    }
}
