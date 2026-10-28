// SPDX-License-Identifier: GPL-3.0-only
//! Required bundled provider, selected lazily for every Windows PTY launch.
use super::*;
use std::fs::File;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::sync::OnceLock;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
    LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{HRESULT, PCSTR};
#[path = "integrity.rs"]
mod integrity;

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> HRESULT;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> HRESULT;
type Close = unsafe extern "system" fn(HPCON);

pub(super) struct Api {
    create: Create,
    resize: Resize,
    close: Close,
    _files: Vec<File>,
    base: Option<PathBuf>,
}
#[derive(Clone, Copy)]
pub(super) struct Provider(&'static Api);
impl Provider {
    pub fn bundled() -> Result<Self> {
        static API: OnceLock<std::result::Result<Api, String>> = OnceLock::new();
        match API.get_or_init(|| load().map_err(|_| {
            "Bundled ConPTY 1.25.260930003 is missing, changed, locked, or could not load. Close OdyTTY and reinstall the complete Windows package folder, including runtime and licenses. Source builds must run scripts/conpty-runtime/Stage.ps1 before launch.".into()
        })) {
            Ok(api) => {
                // File identities stay bound, but a new shadow entry must also
                // be refused on later launches in this same process.
                let base = api.base.as_ref().context("bundled runtime directory")?;
                integrity::layout(base).map_err(|_| anyhow::anyhow!(
                    "Bundled ConPTY runtime layout changed. Close OdyTTY and reinstall the complete Windows package folder."
                ))?;
                Ok(Self(api))
            },
            Err(error) => anyhow::bail!("{error}"),
        }
    }
    pub unsafe fn create(
        self,
        size: COORD,
        input: HANDLE,
        output: HANDLE,
    ) -> windows::core::Result<HPCON> {
        let mut handle = HPCON::default();
        // SAFETY: caller supplies live pipes; all operations use this same ABI.
        unsafe {
            (self.0.create)(size, input, output, 0, &mut handle).ok()?;
        }
        Ok(handle)
    }
    pub unsafe fn resize(self, handle: HPCON, size: COORD) -> windows::core::Result<()> {
        // SAFETY: caller serializes close/resize and retains the creating API.
        unsafe { (self.0.resize)(handle, size).ok() }
    }
    pub unsafe fn close(self, handle: HPCON) {
        // SAFETY: owner guarantees exact-once close through the creating API.
        unsafe { (self.0.close)(handle) }
    }
}
fn load() -> Result<Api> {
    let executable = std::env::current_exe()?.canonicalize()?;
    let bound = integrity::bind(executable.parent().context("executable parent")?)?;
    let wide = to_wide_nul(bound.dll.as_os_str());
    // SAFETY: absolute identity-checked DLL, restricted dependency search. The
    // module is intentionally pinned even on export failure. No API or HPCON
    // can outlive code; held files deny replacement for the process lifetime.
    unsafe {
        let module = LoadLibraryExW(
            PCWSTR(wide.as_ptr()),
            None,
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        )?;
        // Preserve bindings even if a subsequent check fails after module load.
        let files = Box::leak(Box::new(bound.handles));
        let mut loaded = [0; 1024];
        let count = GetModuleFileNameW(Some(module), &mut loaded) as usize;
        anyhow::ensure!(
            count > 0 && count < loaded.len(),
            "loaded runtime path length"
        );
        let loaded = PathBuf::from(OsString::from_wide(&loaded[..count])).canonicalize()?;
        anyhow::ensure!(
            loaded == bound.dll.canonicalize()?,
            "loaded runtime path mismatch"
        );
        let create = GetProcAddress(module, PCSTR(c"ConptyCreatePseudoConsole".as_ptr().cast()))
            .context("ConPTY create export")?;
        let resize = GetProcAddress(module, PCSTR(c"ConptyResizePseudoConsole".as_ptr().cast()))
            .context("ConPTY resize export")?;
        let close = GetProcAddress(module, PCSTR(c"ConptyClosePseudoConsole".as_ptr().cast()))
            .context("ConPTY close export")?;
        Ok(Api {
            create: std::mem::transmute::<unsafe extern "system" fn() -> isize, Create>(create),
            resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, Resize>(resize),
            close: std::mem::transmute::<unsafe extern "system" fn() -> isize, Close>(close),
            _files: std::mem::take(files),
            base: Some(bound.dll.parent().context("runtime DLL parent")?.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    static CREATES: AtomicUsize = AtomicUsize::new(0);
    static RESIZES: AtomicUsize = AtomicUsize::new(0);
    static CLOSES: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "system" fn create(
        _: COORD,
        _: HANDLE,
        _: HANDLE,
        flags: u32,
        output: *mut HPCON,
    ) -> HRESULT {
        assert_eq!(flags, 0);
        CREATES.fetch_add(1, Ordering::SeqCst);
        // SAFETY: adapter supplies a live output slot; no OS handle is touched.
        unsafe {
            *output = HPCON(7);
        }
        HRESULT(0)
    }
    unsafe extern "system" fn resize(handle: HPCON, _: COORD) -> HRESULT {
        assert_eq!(handle.0, 7);
        RESIZES.fetch_add(1, Ordering::SeqCst);
        HRESULT(0)
    }
    unsafe extern "system" fn close(handle: HPCON) {
        assert_eq!(handle.0, 7);
        CLOSES.fetch_add(1, Ordering::SeqCst);
    }
    static MOCK: Api = Api {
        create,
        resize,
        close,
        _files: Vec::new(),
        base: None,
    };
    #[test]
    fn originating_provider_covers_resize_exact_once_and_early_error() {
        let provider = Provider(&MOCK);
        // SAFETY: injected ABI functions never touch OS handles.
        let handle = unsafe {
            provider.create(COORD { X: 80, Y: 24 }, HANDLE::default(), HANDLE::default())
        }
        .unwrap();
        let shared = PconShared::with_provider(handle.0, provider);
        shared.resize(COORD { X: 81, Y: 24 }).unwrap();
        shared.close_once();
        shared.close_once();
        shared.resize(COORD { X: 82, Y: 24 }).unwrap();
        drop(HpconGuard(HPCON(7), provider));
        assert_eq!(CREATES.load(Ordering::SeqCst), 1);
        assert_eq!(RESIZES.load(Ordering::SeqCst), 1);
        assert_eq!(CLOSES.load(Ordering::SeqCst), 2);
    }
}
