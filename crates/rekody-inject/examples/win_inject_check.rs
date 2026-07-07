//! Windows runtime check for text injection.
//!
//! Proves rekody's clipboard + `SendInput(Ctrl+V)` path actually deposits a
//! transcript into a *real*, focused Win32 edit control — the injection analog
//! of `rekody-hotkey`'s `win_hook_check`. It creates a top-level window with a
//! child EDIT, gives the EDIT focus, runs the public `inject_text` API, then
//! reads the control's text back with `WM_GETTEXT` and asserts it round-tripped
//! verbatim. Building + compiling proves the port; *running* proves the paste
//! genuinely lands in a foreign focused control on real Windows.
//!
//! Exit code: 0 = every injection round landed the transcript, 1 = a round
//! failed, 2 = the worker never ran. CI runs this on windows-latest.

#[cfg(not(windows))]
fn main() {
    println!("win_inject_check is Windows-only; nothing to do on this platform.");
}

#[cfg(windows)]
fn main() {
    std::process::exit(windows_only::run());
}

#[cfg(windows)]
mod windows_only {
    use core::ffi::c_void;
    use rekody_inject::{InjectionMethod, inject_text};
    use std::sync::atomic::{AtomicI32, AtomicIsize, Ordering};
    use std::time::Duration;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow,
        GetMessageW, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOW, SendMessageW,
        SetForegroundWindow, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_CLOSE, WM_CREATE,
        WM_DESTROY, WM_GETTEXT, WM_SETTEXT, WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW,
        WS_VISIBLE,
    };
    use windows::core::PCWSTR;

    /// A transcript with mixed case, punctuation, spaces, digits and non-ASCII
    /// glyphs — the clipboard path must carry every one of them verbatim.
    const TEST_TEXT: &str = "rekody windows inject check — 123 ✓";

    static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
    static EDIT_HWND: AtomicIsize = AtomicIsize::new(0);
    static RESULT_CODE: AtomicI32 = AtomicI32::new(2); // 2 = worker never ran

    /// UTF-16, null-terminated — the shape Win32 `W` APIs expect.
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn as_hwnd(v: isize) -> HWND {
        HWND(v as *mut c_void)
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CREATE => {
                // Child EDIT control — the target the transcript pastes into.
                let edit_class = wide("EDIT");
                let empty = wide("");
                let hmod = unsafe { GetModuleHandleW(PCWSTR::null()) }.unwrap_or_default();
                let edit = unsafe {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        PCWSTR(edit_class.as_ptr()),
                        PCWSTR(empty.as_ptr()),
                        WS_CHILD | WS_VISIBLE | WS_BORDER,
                        10,
                        10,
                        380,
                        120,
                        hwnd,
                        None,
                        HINSTANCE(hmod.0),
                        None,
                    )
                }
                .expect("create EDIT control");
                EDIT_HWND.store(edit.0 as isize, Ordering::SeqCst);
                // Focus the edit on the GUI thread so injected keystrokes route
                // to it once the top-level window is foreground.
                unsafe {
                    let _ = SetFocus(edit);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn set_edit_text(edit: HWND, s: &str) {
        let buf = wide(s);
        unsafe {
            SendMessageW(edit, WM_SETTEXT, WPARAM(0), LPARAM(buf.as_ptr() as isize));
        }
    }

    fn get_edit_text(edit: HWND) -> String {
        let mut buf = [0u16; 1024];
        let n = unsafe {
            SendMessageW(
                edit,
                WM_GETTEXT,
                WPARAM(buf.len()),
                LPARAM(buf.as_mut_ptr() as isize),
            )
        };
        let len = (n.0.max(0) as usize).min(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    /// One injection round: clear the edit, foreground the window, run the
    /// public `inject_text`, then read the control's text back.
    fn round(label: &str, edit: HWND, main: HWND, method: InjectionMethod) -> bool {
        set_edit_text(edit, "");
        unsafe {
            let _ = SetForegroundWindow(main);
        }
        std::thread::sleep(Duration::from_millis(150));

        let injected = inject_text(TEST_TEXT, method);
        // Let the GUI thread dispatch the synthesized Ctrl+V to the edit and
        // let the paste settle before reading it back.
        std::thread::sleep(Duration::from_millis(500));

        let got = get_edit_text(edit);
        let fg = unsafe { GetForegroundWindow() };
        let ok = injected.is_ok() && got == TEST_TEXT;
        println!(
            "  [{label}] inject={:?} foreground_is_probe={} got={:?}",
            injected.as_ref().map(|_| "ok"),
            fg.0 == main.0,
            got
        );
        if !ok {
            eprintln!("  [{label}] FAIL — want {TEST_TEXT:?}");
        }
        ok
    }

    fn worker() {
        // Wait for the GUI thread to publish both window handles.
        let (main, edit) = loop {
            let m = MAIN_HWND.load(Ordering::SeqCst);
            let e = EDIT_HWND.load(Ordering::SeqCst);
            if m != 0 && e != 0 {
                break (as_hwnd(m), as_hwnd(e));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Let the window fully realize and come to the foreground.
        std::thread::sleep(Duration::from_millis(600));

        println!("== win_inject_check ==");
        let native = round("native", edit, main, InjectionMethod::Native);
        let clip = round("clipboard", edit, main, InjectionMethod::Clipboard);

        let code = if native && clip { 0 } else { 1 };
        RESULT_CODE.store(code, Ordering::SeqCst);
        if code == 0 {
            println!("win_inject_check: all injection rounds landed the transcript ✓");
        } else {
            eprintln!("win_inject_check: FAILED (native={native} clipboard={clip})");
        }

        // Break the GUI message loop so run() can return.
        unsafe {
            let _ = PostMessageW(main, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    pub fn run() -> i32 {
        let hmod = unsafe { GetModuleHandleW(PCWSTR::null()) }.expect("module handle");
        let hinst = HINSTANCE(hmod.0);

        let class_name = wide("rekody_inject_probe");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        let atom = unsafe { RegisterClassW(&wc) };
        assert!(atom != 0, "RegisterClassW failed");

        let title = wide("rekody inject probe");
        let main = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                420,
                220,
                None,
                None,
                hinst,
                None,
            )
        }
        .expect("create main window");
        MAIN_HWND.store(main.0 as isize, Ordering::SeqCst);
        unsafe {
            let _ = ShowWindow(main, SW_SHOW);
            let _ = SetForegroundWindow(main);
        }

        let worker = std::thread::spawn(worker);

        let mut msg = MSG::default();
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        let _ = worker.join();
        RESULT_CODE.load(Ordering::SeqCst)
    }
}
