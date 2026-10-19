// monctl-tray — Windows menu bar (system tray) app. Windows-only; builds to a
// stub on other targets so `cargo build` works everywhere.
//
// Raw win32, zero dependencies: Shell_NotifyIconW + a hidden message window,
// same toggles as the CLI, same last-screen guard, same state files.
//
// The tray runs in the logged-on user's session (display APIs are
// session-scoped on Windows — never start it from a service context).

#[cfg(windows)]
mod tray {
    use monctl::common::{self, DisplayInfo, OffState};
    use std::ffi::c_void;
    use std::path::PathBuf;

    type DWORD = u32;
    type WCHAR = u16;
    type UINT = u32;
    type BOOL = i32;
    type WPARAM = usize;
    type LPARAM = isize;
    type LRESULT = isize;
    type HANDLE = *mut c_void;
    type HWND = HANDLE;
    type HMENU = HANDLE;
    type HICON = HANDLE;
    type HINSTANCE = HANDLE;
    type HKEY = HANDLE;
    type HDC = HANDLE;
    type HBITMAP = HANDLE;
    type HGDIOBJ = HANDLE;

    #[repr(C)]
    struct RECT {
        l: i32,
        t: i32,
        r: i32,
        b: i32,
    }

    #[repr(C)]
    struct ICONINFO {
        f_icon: BOOL,
        x_hotspot: DWORD,
        y_hotspot: DWORD,
        mask: HBITMAP,
        color: HBITMAP,
    }

    const WM_APP: UINT = 0x8000;
    const WM_LBUTTONUP: UINT = 0x0202;
    const WM_RBUTTONUP: UINT = 0x0205;
    const WM_CLOSE: UINT = 0x0010;
    const WM_DESTROY: UINT = 0x0002;
    const WM_NULL: UINT = 0x0000;
    const WM_COMMAND: UINT = 0x0111;
    const WM_USER_TRAY: UINT = WM_APP + 1;
    const WM_HOTKEY: UINT = 0x0312;
    // global recovery hotkey: Ctrl+Alt+Shift+D = enable all displays
    const HOTKEY_MODS: UINT = 0x2 | 0x1 | 0x4; // MOD_CONTROL | MOD_ALT | MOD_SHIFT
    const HOTKEY_VK: UINT = 0x44; // 'D'
    const NIM_ADD: DWORD = 0;
    const NIM_MODIFY: DWORD = 1;
    const NIM_DELETE: DWORD = 2;
    const NIF_MESSAGE: DWORD = 0x1;
    const NIF_ICON: DWORD = 0x2;
    const NIF_TIP: DWORD = 0x4;
    const NIF_INFO: DWORD = 0x10;
    const CS_VREDRAW: DWORD = 0x1;
    const CS_HREDRAW: DWORD = 0x2;
    const WM_APP_DESTROY: UINT = WM_APP + 2;
    const TPM_RIGHTBUTTON: UINT = 0x2;
    const TPM_RETURNCMD: UINT = 0x100;
    const MF_STRING: UINT = 0x0;
    const MF_SEPARATOR: UINT = 0x800;
    const MF_GRAYED: UINT = 0x1;
    const MF_CHECKED: UINT = 0x8;
    const KEY_SET_VALUE: DWORD = 0x0002;
    const KEY_QUERY_VALUE: DWORD = 0x0001;
    const REG_SZ: DWORD = 1;

    // menu ids
    const ID_DISPLAY_BASE: u32 = 1000; // +index into snapshot
    const ID_SOLO: u32 = 1;
    const ID_ALL: u32 = 2;
    const ID_AWAKE_BASE: u32 = 10; // +0..=3 durations, 4 = off
    const ID_AUTOSTART: u32 = 20;
    const ID_QUIT: u32 = 21;
    const AWAKE_SECS: [(&str, u64); 4] = [("1 小时", 3600), ("4 小时", 14400), ("8 小时", 28800), ("不限时", 0)];

    #[repr(C)]
    struct WNDCLASSEXW {
        cb_size: UINT,
        style: UINT,
        wnd_proc: unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT,
        cls_extra: i32,
        wnd_extra: i32,
        instance: HINSTANCE,
        icon: HICON,
        cursor: HANDLE,
        background: HANDLE,
        menu_name: *const WCHAR,
        class_name: *const WCHAR,
        icon_sm: HICON,
    }

    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct MSG {
        hwnd: HWND,
        message: UINT,
        w_param: WPARAM,
        l_param: LPARAM,
        time: DWORD,
        pt: POINT,
    }

    #[repr(C)]
    struct NOTIFYICONDATAW {
        cb_size: DWORD,
        hwnd: HWND,
        uid: DWORD,
        uflags: DWORD,
        callback_message: UINT,
        icon: HICON,
        tip: [WCHAR; 128],
        state: DWORD,
        state_mask: DWORD,
        info: [WCHAR; 256],
        version: UINT,
        info_title: [WCHAR; 64],
        info_flags: DWORD,
        guid: [u8; 16],
        balloon_icon: HICON,
    }

    impl NOTIFYICONDATAW {
        fn new(hwnd: HWND) -> Self {
            let mut n: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
            n.cb_size = std::mem::size_of::<NOTIFYICONDATAW>() as DWORD;
            n.hwnd = hwnd;
            n.uid = 1;
            n.uflags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            n.callback_message = WM_USER_TRAY;
            n
        }
    }

    #[link(name = "user32")]
    extern "system" {
        fn RegisterWindowMessageW(text: *const WCHAR) -> UINT;
        fn RegisterClassExW(wc: *const WNDCLASSEXW) -> u16;
        fn CreateWindowExW(exstyle: DWORD, class: *const WCHAR, window: *const WCHAR, style: DWORD, x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: HMENU, instance: HINSTANCE, param: *mut c_void) -> HWND;
        fn GetMessageW(msg: *mut MSG, hwnd: HWND, min: UINT, max: UINT) -> BOOL;
        fn TranslateMessage(msg: *const MSG) -> BOOL;
        fn DispatchMessageW(msg: *const MSG) -> LRESULT;
        fn DefWindowProcW(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT;
        fn PostQuitMessage(code: i32);
        fn PostMessageW(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> BOOL;
        fn GetDC(hwnd: HWND) -> HDC;
        fn ReleaseDC(hwnd: HWND, dc: HDC) -> i32;
        fn FillRect(dc: HDC, rect: *const RECT, brush: HANDLE) -> i32;
        fn SetBkMode(dc: HDC, mode: i32) -> i32;
        fn SetTextColor(dc: HDC, color: u32) -> u32;
        fn DrawTextW(dc: HDC, text: *mut WCHAR, len: i32, rect: *mut RECT, flags: UINT) -> i32;
        fn CreatePopupMenu() -> HMENU;
        fn AppendMenuW(menu: HMENU, flags: UINT, id: WPARAM, text: *const WCHAR) -> BOOL;
        fn TrackPopupMenu(menu: HMENU, flags: UINT, x: i32, y: i32, reserved: i32, hwnd: HWND, rect: *const c_void) -> BOOL;
        fn DestroyMenu(menu: HMENU) -> BOOL;
        fn GetCursorPos(pt: *mut POINT) -> BOOL;
        fn SetForegroundWindow(hwnd: HWND) -> BOOL;
        fn RegisterHotKey(hwnd: HWND, id: i32, modifiers: UINT, vk: UINT) -> BOOL;
        fn UnregisterHotKey(hwnd: HWND, id: i32) -> BOOL;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn Shell_NotifyIconW(msg: DWORD, data: *mut NOTIFYICONDATAW) -> BOOL;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn RegCreateKeyExW(key: HKEY, subkey: *const WCHAR, reserved: DWORD, class: *const WCHAR, options: DWORD, access: DWORD, security: *const c_void, result: *mut HKEY, disposition: *mut DWORD) -> i32;
        fn RegOpenKeyExW(key: HKEY, subkey: *const WCHAR, reserved: DWORD, access: DWORD, result: *mut HKEY) -> i32;
        fn RegSetValueExW(key: HKEY, name: *const WCHAR, reserved: DWORD, kind: DWORD, data: *const u8, size: DWORD) -> i32;
        fn RegDeleteValueW(key: HKEY, name: *const WCHAR) -> i32;
        fn RegQueryValueExW(key: HKEY, name: *const WCHAR, reserved: *const DWORD, kind: *mut DWORD, buf: *mut u8, size: *mut DWORD) -> i32;
        fn RegCloseKey(key: HKEY) -> i32;
    }

    const HKEY_CURRENT_USER: HKEY = 0x8000_0001usize as HKEY;

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(dc: HDC) -> HDC;
        fn CreateCompatibleBitmap(dc: HDC, w: i32, h: i32) -> HBITMAP;
        fn CreateSolidBrush(color: u32) -> HANDLE;
        fn CreateFontW(height: i32, width: i32, esc: i32, orient: i32, weight: i32, italic: DWORD, underline: DWORD, strike: DWORD, charset: DWORD, out_prec: DWORD, clip_prec: DWORD, quality: DWORD, pitch_family: DWORD, face: *const WCHAR) -> HANDLE;
        fn SelectObject(dc: HDC, obj: HGDIOBJ) -> HGDIOBJ;
        fn DeleteObject(obj: HGDIOBJ) -> BOOL;
        fn DeleteDC(dc: HDC) -> BOOL;
        fn CreateBitmap(w: i32, h: i32, planes: UINT, bits: UINT, data: *const c_void) -> HBITMAP;
    }

    #[link(name = "user32")]
    extern "system" {
        fn CreateIconIndirect(info: *const ICONINFO) -> HICON;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> HANDLE;
        fn IsProcessInJob(process: HANDLE, job: HANDLE, result: *mut BOOL) -> BOOL;
    }

    /// A recognizable tray icon: blue rounded square with a white "M".
    /// (The stock IDI_APPLICATION white window is invisible among real apps.)
    unsafe fn make_icon() -> HICON {
        let size = 32;
        let dc_screen = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(dc_screen);
        let bmp = CreateCompatibleBitmap(dc_screen, size, size);
        let old_bmp = SelectObject(mem, bmp);

        let brush = CreateSolidBrush(0x00E6_6C1E); // RGB(30,108,230) as COLORREF
        let rect = RECT { l: 0, t: 0, r: size, b: size };
        FillRect(mem, &rect, brush);
        DeleteObject(brush);

        let font = CreateFontW(-22, 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, 0, 0, wide("Segoe UI").as_ptr());
        let old_font = SelectObject(mem, font);
        SetBkMode(mem, 1); // TRANSPARENT
        SetTextColor(mem, 0x00FF_FFFF);
        let mut tr = RECT { l: 0, t: 2, r: size, b: size };
        let mut text = wide("M");
        DrawTextW(mem, text.as_mut_ptr(), 1, &mut tr, 0x1 | 0x4 | 0x20); // CENTER|VCENTER|SINGLELINE
        SelectObject(mem, old_font);
        DeleteObject(font);

        SelectObject(mem, old_bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), dc_screen);

        let mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let ii = ICONINFO { f_icon: 1, x_hotspot: 0, y_hotspot: 0, mask, color: bmp };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(bmp);
        DeleteObject(mask);
        icon
    }

    fn wide(s: &str) -> Vec<WCHAR> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(w: &[WCHAR]) -> String {
        let len = w.iter().position(|&c| c == 0).unwrap_or(w.len());
        String::from_utf16_lossy(&w[..len])
    }

    // ---- tray state (single UI thread) ----

    thread_local! {
        static UI: std::cell::RefCell<Ui> = std::cell::RefCell::new(Ui::default());
    }

    #[derive(Default, Clone)]
    struct Ui {
        hwnd: HWND,
        snapshot: Vec<DisplayInfo>,
        remember: OffState,
    }

    const REMEMBER_BASE: u32 = 500; // menu id offset for remembered-off displays

    fn exe_path() -> PathBuf {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("monctl-tray.exe"))
    }

    fn monctl_exe() -> Option<PathBuf> {
        let sibling = exe_path().parent()?.join("monctl.exe");
        if sibling.is_file() { Some(sibling) } else { None }
    }

    fn autostart_enabled() -> bool {
        let (key, name) = run_key();
        let mut kind: DWORD = 0;
        let mut size: DWORD = 0;
        let ok = unsafe {
            RegQueryValueExW(key, wide(&name).as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut size) == 0
        };
        ok
    }

    fn set_autostart(on: bool) {
        let (key, name) = run_key();
        unsafe {
            let mut hk: HKEY = std::ptr::null_mut();
            if RegCreateKeyExW(key, wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run").as_ptr(), 0, std::ptr::null(), 0, KEY_SET_VALUE, std::ptr::null(), &mut hk, std::ptr::null_mut()) != 0 {
                return;
            }
            if on {
                let path = wide(&exe_path().to_string_lossy());
                RegSetValueExW(hk, wide(&name).as_ptr(), 0, REG_SZ, path.as_ptr() as *const u8, (path.len() * 2) as DWORD);
            } else {
                RegDeleteValueW(hk, wide(&name).as_ptr());
            }
            RegCloseKey(hk);
        }
    }

    fn run_key() -> (HKEY, String) {
        (HKEY_CURRENT_USER, "monctl-tray".to_string())
    }

    fn spawn_monctl(args: &[&str]) {
        if let Some(exe) = monctl_exe() {
            use std::os::windows::process::CommandExt;
            use std::process::{Command, Stdio};
            const CREATE_NO_WINDOW: DWORD = 0x0800_0000;
            let _ = Command::new(exe)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }

    fn awake_remaining() -> Option<u64> {
        let p = common::awake_path();
        let s = std::fs::read_to_string(p).ok()?;
        let get = |k: &str| -> Option<u64> {
            let marker = format!("\"{k}\":");
            let i = s.find(&marker)? + marker.len();
            let rest = &s[i..];
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            rest[..end].parse().ok()
        };
        if get("release") == Some(1) {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let heartbeat = get("heartbeat")?;
        if now.saturating_sub(heartbeat) > 15 {
            return None;
        }
        Some(get("deadline")?.saturating_sub(now))
    }

    fn fmt_secs(s: u64) -> String {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if h > 0 { format!("{h}h{m:02}m") } else { format!("{m}m") }
    }

    unsafe fn build_menu(displays: &[DisplayInfo], remember: &OffState) -> (HMENU, bool) {
        let menu = CreatePopupMenu();
        let active_count = displays.iter().filter(|d| d.active).count();
        let any_off_remembered = remember.keys().any(|id| !displays.iter().any(|d| d.id == *id && d.active));

        for (i, d) in displays.iter().enumerate() {
            // only actionable rows: active displays, plus adapters WE turned off.
            // everything else (GPU outputs with no panel, remote-tool virtual
            // displays, emulator leftovers) is registry noise — hide it.
            let monctl_off = remember.contains_key(&d.id);
            if !d.active && !monctl_off {
                continue;
            }
            let mut title = if d.active {
                format!("关闭  {}", d.name)
            } else {
                format!("开启  {}", d.name)
            };
            if d.main && d.active {
                title.push_str("（主屏）");
            }
            let mut flags = MF_STRING;
            if d.active && active_count <= 1 {
                title.push_str(" — 最后一块，不能关");
                flags |= MF_GRAYED;
            }
            AppendMenuW(menu, flags, (ID_DISPLAY_BASE + i as u32) as WPARAM, wide(&title).as_ptr());
        }
        let seen = |id: &str| displays.iter().any(|d| d.id == id);
        for (i, (id, name)) in remember.iter().enumerate() {
            if !seen(id) {
                AppendMenuW(menu, MF_STRING, (REMEMBER_BASE + i as u32) as WPARAM, wide(&format!("开启  {name}")).as_ptr());
            }
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        let mut it = MF_STRING;
        if active_count <= 1 {
            it |= MF_GRAYED;
        }
        AppendMenuW(menu, it, ID_SOLO as WPARAM, wide("只留主屏").as_ptr());
        let all_flags = if active_count == displays.len() && !any_off_remembered { MF_STRING | MF_GRAYED } else { MF_STRING };
        AppendMenuW(menu, all_flags, ID_ALL as WPARAM, wide("全部开启 (Ctrl+Alt+Shift+D)").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        let remaining = awake_remaining();
        let awake_label = match remaining {
            Some(r) => format!("保持唤醒（剩余 {}）", fmt_secs(r)),
            None => "保持唤醒".to_string(),
        };
        AppendMenuW(menu, MF_STRING | MF_GRAYED, ID_AWAKE_BASE as WPARAM, wide(&awake_label).as_ptr());
        for (i, (label, _)) in AWAKE_SECS.iter().enumerate() {
            AppendMenuW(menu, MF_STRING, (ID_AWAKE_BASE + 10 + i as u32) as WPARAM, wide(label).as_ptr());
        }
        let off_flags = MF_STRING | if remaining.is_some() { 0 } else { MF_GRAYED };
        AppendMenuW(menu, off_flags, (ID_AWAKE_BASE + 14) as WPARAM, wide("解除保持").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        let auto_flags = MF_STRING | if autostart_enabled() { MF_CHECKED } else { 0 };
        AppendMenuW(menu, auto_flags, ID_AUTOSTART as WPARAM, wide("开机启动").as_ptr());
        AppendMenuW(menu, MF_STRING, ID_QUIT as WPARAM, wide("退出").as_ptr());
        (menu, true)
    }

    unsafe fn handle_click(id: u32, hwnd: HWND) {
        let _ = hwnd;
        if id >= REMEMBER_BASE && id < ID_DISPLAY_BASE {
            // remembered-off display that vanished from the OS list: enable by remembered id
            let remember = UI.with(|u| u.borrow().remember.clone());
            let pos = (id - REMEMBER_BASE) as usize;
            let ids: Vec<String> = remember.keys().cloned().collect();
            if let Some(key) = ids.get(pos) {
                let key = key.clone();
                std::thread::spawn(move || {
                    let _ = monctl::windows::set_enabled(&key, true);
                    let mut st = common::load_state();
                    st.remove(&key);
                    common::save_state(&st);
                });
            }
            return;
        }
        if id >= ID_DISPLAY_BASE {
            let ui = UI.with(|u| u.borrow().clone());
            let idx = (id - ID_DISPLAY_BASE) as usize;
            if let Some(d) = ui.snapshot.get(idx) {
                let d = d.clone();
                std::thread::spawn(move || {
                    if d.active && monctl::windows::list().iter().filter(|x| x.active).count() <= 1 {
                        return; // last-screen guard (menu item is grayed; belt & suspenders)
                    }
                    match monctl::windows::set_enabled(&d.id, !d.active) {
                        Ok(()) => {
                            let mut st = common::load_state();
                            if d.active {
                                st.insert(d.id.clone(), d.name.clone());
                            } else {
                                st.remove(&d.id);
                            }
                            common::save_state(&st);
                        }
                        Err(e) => {
                            // keep a trace; a balloon would be overkill for a background toggle
                            let log = common::state_path().with_file_name("tray-error.log");
                            let _ = std::fs::write(log, format!("{e}\n"));
                        }
                    }
                });
            }
            return;
        }
        match id {
            ID_SOLO => {
                std::thread::spawn(|| {
                    let displays = monctl::windows::list();
                    for d in displays.iter().filter(|d| d.active && !d.main) {
                        if monctl::windows::set_enabled(&d.id, false).is_ok() {
                            let mut st = common::load_state();
                            st.insert(d.id.clone(), d.name.clone());
                            common::save_state(&st);
                        }
                    }
                });
            }
            ID_ALL => {
                std::thread::spawn(|| {
                    let displays = monctl::windows::list();
                    let mut ids: Vec<String> = displays.iter().filter(|d| !d.active).map(|d| d.id.clone()).collect();
                    ids.extend(common::load_state().keys().cloned());
                    ids.retain(|id| !displays.iter().any(|d| d.id == *id && d.active));
                    for id in ids {
                        if monctl::windows::set_enabled(&id, true).is_ok() {
                            let mut st = common::load_state();
                            st.remove(&id);
                            common::save_state(&st);
                        }
                    }
                });
            }
            ID_AWAKE_BASE => {}
            i if i >= ID_AWAKE_BASE + 10 && i <= ID_AWAKE_BASE + 13 => {
                let secs = AWAKE_SECS[(i - ID_AWAKE_BASE - 10) as usize].1;
                if secs == 0 {
                    spawn_monctl(&["awake"]);
                } else {
                    spawn_monctl(&["awake", &secs.to_string()]);
                }
            }
            i if i == ID_AWAKE_BASE + 14 => spawn_monctl(&["awake", "off"]),
            ID_AUTOSTART => {
                let on = !autostart_enabled();
                set_autostart(on);
            }
            ID_QUIT => {
                UnregisterHotKey(hwnd, 1);
                let mut n = NOTIFYICONDATAW::new(hwnd);
                Shell_NotifyIconW(NIM_DELETE, &mut n);
                PostQuitMessage(0);
            }
            _ => {}
        }
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // explorer/taskbar restarted: re-register the icon or it silently disappears
        if msg == taskbar_created_msg() {
            readd_icon(hwnd);
            return 0;
        }
        if msg == WM_USER_TRAY {
            let up = lp as DWORD as UINT;
            if up == WM_LBUTTONUP || up == WM_RBUTTONUP {
                UI.with(|u| {
                    let mut ui = u.borrow_mut();
                    ui.snapshot = monctl::windows::list();
                    ui.remember = common::load_state();
                    let (menu, _) = build_menu(&ui.snapshot, &ui.remember);
                    let mut pt = POINT { x: 0, y: 0 };
                    GetCursorPos(&mut pt);
                    SetForegroundWindow(hwnd); // classic trick so the menu dismisses on outside click
                    let chosen = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, 0, hwnd, std::ptr::null());
                    DestroyMenu(menu);
                    if chosen != 0 {
                        PostMessageW(hwnd, WM_COMMAND, chosen as WPARAM, 0);
                    }
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                });
            }
            return 0;
        }
        if msg == WM_COMMAND {
            handle_click(wp as u32, hwnd);
            return 0;
        }
        if msg == WM_HOTKEY && wp == 1 {
            // blind-safe recovery: Ctrl+Alt+Shift+D re-enables every display
            handle_click(ID_ALL, hwnd);
            return 0;
        }
        if msg == WM_APP_DESTROY {
            PostQuitMessage(0);
            return 0;
        }
        if msg == WM_CLOSE || msg == WM_DESTROY {
            PostQuitMessage(0);
            return 0;
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    extern "system" fn wnd_proc_shim(hwnd: HWND, msg: UINT, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe { wnd_proc(hwnd, msg, wp, lp) }
    }

    fn taskbar_created_msg() -> UINT {
        thread_local! {
            static MSG: UINT = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
        }
        MSG.with(|m| *m)
    }

    unsafe fn readd_icon(hwnd: HWND) {
        let mut n = NOTIFYICONDATAW::new(hwnd);
        n.icon = make_icon();
        let tip = wide("monctl");
        let tlen = tip.len().min(127);
        n.tip[..tlen].copy_from_slice(&tip[..tlen]);
        Shell_NotifyIconW(NIM_ADD, &mut n);
    }

    /// Scheduled-task instances run inside the task's job object; deleting the
    /// task (or the job being finalized) can take the tray down with it.
    /// If we were started inside a job, respawn ourselves with
    /// CREATE_BREAKAWAY_FROM_JOB and exit — the free-standing copy outlives
    /// whatever launched us. Guarded by an env var so the child doesn't loop.
    fn escape_job_if_needed() {
        if std::env::var_os("MONCTL_TRAY_NO_ESCAPE").is_some() {
            return;
        }
        let mut in_job: BOOL = 0;
        unsafe {
            IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job);
        }
        if in_job == 0 {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const DETACHED_PROCESS: DWORD = 0x0000_0008;
        const CREATE_NO_WINDOW: DWORD = 0x0800_0000;
        const CREATE_BREAKAWAY_FROM_JOB: DWORD = 0x0100_0000;
        let child = Command::new(exe)
            .env("MONCTL_TRAY_NO_ESCAPE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB)
            .spawn();
        if child.is_ok() {
            std::process::exit(0); // handover to the free-standing copy
        }
        // breakaway refused by the job: stay put and hope for the best
    }

    pub fn run() -> std::process::ExitCode {
        escape_job_if_needed();
        unsafe {
            let class_name = wide("monctl_tray_wnd");
            let hinstance = std::ptr::null_mut();
            let wc = WNDCLASSEXW {
                cb_size: std::mem::size_of::<WNDCLASSEXW>() as UINT,
                style: CS_VREDRAW | CS_HREDRAW,
                wnd_proc: wnd_proc_shim,
                cls_extra: 0,
                wnd_extra: 0,
                instance: hinstance,
                icon: std::ptr::null_mut(),
                cursor: std::ptr::null_mut(),
                background: std::ptr::null_mut(),
                menu_name: std::ptr::null(),
                class_name: class_name.as_ptr(),
                icon_sm: std::ptr::null_mut(),
            };
            if RegisterClassExW(&wc) == 0 {
                return std::process::ExitCode::FAILURE;
            }
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                wide("monctl-tray").as_ptr(),
                0, // not visible; icon only
                0, 0, 0, 0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                return std::process::ExitCode::FAILURE;
            }
            let mut n = NOTIFYICONDATAW::new(hwnd);
            n.icon = make_icon();
            let tip = wide("monctl");
            let tlen = tip.len().min(127);
            n.tip[..tlen].copy_from_slice(&tip[..tlen]);
            if Shell_NotifyIconW(NIM_ADD, &mut n) == 0 {
                return std::process::ExitCode::FAILURE;
            }
            // one-shot balloon so people can locate the icon (often hidden in the ^ overflow)
            let mut b = NOTIFYICONDATAW::new(hwnd);
            b.uflags |= NIF_INFO;
            b.info_flags = 0x1; // NIIF_INFO
            let title = wide("monctl 托盘已启动");
            let t = title.len().min(63);
            b.info_title[..t].copy_from_slice(&title[..t]);
            let body = wide("图标在任务栏右下角（可能在 ^ 溢出区里的蓝色 M）。右键它管理屏幕和防休眠；Ctrl+Alt+Shift+D 恢复全部显示器。");
            let bl = body.len().min(255);
            b.info[..bl].copy_from_slice(&body[..bl]);
            Shell_NotifyIconW(NIM_MODIFY, &mut b);
            UI.with(|u| u.borrow_mut().hwnd = hwnd);
            RegisterHotKey(hwnd, 1, HOTKEY_MODS, HOTKEY_VK);

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            std::process::ExitCode::SUCCESS
        }
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    let code = tray::run();
    #[cfg(not(windows))]
    let code = {
        eprintln!("monctl-tray is Windows-only (macOS has macos-tray/ instead)");
        std::process::ExitCode::FAILURE
    };
    code
}
