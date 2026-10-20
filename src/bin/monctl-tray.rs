// monctl-tray — Windows menu bar (system tray) app. Windows-only; builds to a
// stub on other targets so `cargo build` works everywhere.
//
// Raw win32, zero dependencies: Shell_NotifyIconW + a hidden message window,
// same toggles as the CLI, same last-screen guard, same state files.
//
// windows_subsystem keeps the tray console-free when double-clicked or
// started from the Run key.
#![windows_subsystem = "windows"]
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
    const WM_TIMER: UINT = 0x0113;
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
    // Fixed items live in 300..400. They used to sit at 1..21, which collided
    // with the awake-duration items at ID_AWAKE_BASE+10..=13 (20/21/22/23):
    // clicking "开机启动" (20) actually started a 1-hour awake hold, and
    // "退出" (21) started a 4-hour one. Displays at 1000+ and remembered-off
    // ghosts at 500..999 stay where they were.
    const ID_SOLO: u32 = 300;
    const ID_ALL: u32 = 301;
    const ID_AWAKE_BASE: u32 = 302; // +0..=3 durations, 4 = off
    const ID_AUTOSTART: u32 = 307;
    const ID_QUIT: u32 = 308;
    const ID_KEEP_DISPLAY: u32 = 309; // one-click screen-sleep prevention toggle
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
        fn CreatePopupMenu() -> HMENU;
        fn AppendMenuW(menu: HMENU, flags: UINT, id: WPARAM, text: *const WCHAR) -> BOOL;
        fn TrackPopupMenu(menu: HMENU, flags: UINT, x: i32, y: i32, reserved: i32, hwnd: HWND, rect: *const c_void) -> BOOL;
        fn DestroyMenu(menu: HMENU) -> BOOL;
        fn GetCursorPos(pt: *mut POINT) -> BOOL;
        fn SetForegroundWindow(hwnd: HWND) -> BOOL;
        fn RegisterHotKey(hwnd: HWND, id: i32, modifiers: UINT, vk: UINT) -> BOOL;
        fn UnregisterHotKey(hwnd: HWND, id: i32) -> BOOL;
        fn SetTimer(hwnd: HWND, id: usize, ms: UINT, proc: *const c_void) -> usize;
        fn KillTimer(hwnd: HWND, id: usize) -> BOOL;
    }

    #[link(name = "shell32")]
    extern "system" {
        fn Shell_NotifyIconW(msg: DWORD, data: *mut NOTIFYICONDATAW) -> BOOL;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn RegCreateKeyExW(key: HKEY, subkey: *const WCHAR, reserved: DWORD, class: *const WCHAR, options: DWORD, access: DWORD, security: *const c_void, result: *mut HKEY, disposition: *mut DWORD) -> i32;
        fn RegSetValueExW(key: HKEY, name: *const WCHAR, reserved: DWORD, kind: DWORD, data: *const u8, size: DWORD) -> i32;
        fn RegDeleteValueW(key: HKEY, name: *const WCHAR) -> i32;
        fn RegOpenKeyExW(key: HKEY, subkey: *const WCHAR, reserved: DWORD, access: DWORD, result: *mut HKEY) -> i32;
        fn RegQueryValueExW(key: HKEY, name: *const WCHAR, reserved: *const DWORD, kind: *mut DWORD, buf: *mut u8, size: *mut DWORD) -> i32;
        fn RegCloseKey(key: HKEY) -> i32;
    }

    const HKEY_CURRENT_USER: HKEY = 0x8000_0001usize as HKEY;

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(dc: HDC) -> HDC;
        fn SelectObject(dc: HDC, obj: HGDIOBJ) -> HGDIOBJ;
        fn DeleteObject(obj: HGDIOBJ) -> BOOL;
        fn DeleteDC(dc: HDC) -> BOOL;
        fn CreateBitmap(w: i32, h: i32, planes: UINT, bits: UINT, data: *const c_void) -> HBITMAP;
        fn SetBitmapBits(bitmap: HBITMAP, bytes: DWORD, bits: *const c_void) -> i32;
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

    #[repr(C)]
    struct BITMAPINFOHEADER {
        bi_size: DWORD,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: DWORD,
        bi_size_image: DWORD,
        bi_x_pels_per_meter: DWORD,
        bi_y_pels_per_meter: DWORD,
        bi_clr_used: DWORD,
        bi_clr_important: DWORD,
    }

    #[repr(C)]
    struct BITMAPINFO {
        header: BITMAPINFOHEADER,
    }

    const BI_RGB: DWORD = 0;
    const DIB_RGB_COLORS: UINT = 0;

    #[link(name = "gdi32")]
    extern "system" {
        fn CreateDIBSection(dc: HDC, bmi: *const BITMAPINFO, usage: UINT, bits: *mut *mut c_void, section: HANDLE, offset: DWORD) -> HBITMAP;
        fn CreatePen(style: i32, width: i32, color: u32) -> HANDLE;
        fn CreateSolidBrush(color: u32) -> HANDLE;
        fn Rectangle(dc: HDC, l: i32, t: i32, r: i32, b: i32) -> BOOL;
        fn GetStockObject(index: i32) -> HGDIOBJ;
    }

    const PS_SOLID: i32 = 0;
    const NULL_BRUSH: i32 = 5;

    /// A recognizable tray icon matching the macOS template style (and native
    /// Windows tray glyphs): two outlined displays on a transparent
    /// background, drawn white.
    ///
    /// Drawn into a 32bpp DIB section. GDI never writes the alpha lane, so
    /// afterwards alpha is derived from pixel luminance: white strokes become
    /// opaque, the untouched background stays fully transparent, and antialiased
    /// edges get partial coverage. A plain DDB (or an uninitialized AND mask)
    /// instead renders as a blob — or not at all.
    unsafe fn make_icon() -> HICON {
        let size = 32i32;
        let dc_screen = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(dc_screen);

        let bmi = BITMAPINFO {
            header: BITMAPINFOHEADER {
                bi_size: std::mem::size_of::<BITMAPINFOHEADER>() as DWORD,
                bi_width: size,
                bi_height: -size, // top-down
                bi_planes: 1,
                bi_bit_count: 32,
                bi_compression: BI_RGB,
                bi_size_image: 0,
                bi_x_pels_per_meter: 0,
                bi_y_pels_per_meter: 0,
                bi_clr_used: 0,
                bi_clr_important: 0,
            },
        };
        let mut px: *mut u8 = std::ptr::null_mut();
        let dib = CreateDIBSection(dc_screen, &bmi, DIB_RGB_COLORS, std::ptr::addr_of_mut!(px) as *mut *mut c_void, std::ptr::null_mut(), 0);
        let old_bmp = SelectObject(mem, dib);

        // transparent background: the freshly zeroed DIB stays untouched
        let pen = CreatePen(PS_SOLID, 2, 0x00FF_FFFF); // white COLORREF
        let black = CreateSolidBrush(0x0000_0000);
        let null_brush = GetStockObject(NULL_BRUSH);
        let old_pen = SelectObject(mem, pen);
        // back display (right, smaller), drawn first...
        SelectObject(mem, null_brush);
        Rectangle(mem, 18, 5, 30, 16);
        // ...front display (left, larger): its opaque-black fill occludes the
        // back strokes inside its silhouette (the alpha pass turns black into
        // transparency), so the pair reads as stacked front-to-back like the
        // macOS `display.2` symbol instead of two touching rectangles.
        SelectObject(mem, black);
        Rectangle(mem, 2, 10, 22, 25);
        SelectObject(mem, old_pen);
        DeleteObject(pen);
        DeleteObject(black);

        // alpha = coverage (white strokes on transparent black); force RGB to white
        if !px.is_null() {
            let pixels = std::slice::from_raw_parts_mut(px, (size * size * 4) as usize);
            for i in 0..(size * size) as usize {
                let (b, g, r) = (pixels[i * 4] as u32, pixels[i * 4 + 1] as u32, pixels[i * 4 + 2] as u32);
                let coverage = b.max(g).max(r).min(255) as u8;
                pixels[i * 4] = 0xFF;
                pixels[i * 4 + 1] = 0xFF;
                pixels[i * 4 + 2] = 0xFF;
                pixels[i * 4 + 3] = coverage;
            }
        }

        SelectObject(mem, old_bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), dc_screen);

        // AND mask all-zero = transparency comes from the alpha lane alone.
        let mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let zeros = [0u8; (32 * 32 / 8) as usize];
        SetBitmapBits(mask, zeros.len() as DWORD, zeros.as_ptr() as *const c_void);

        let ii = ICONINFO { f_icon: 1, x_hotspot: 0, y_hotspot: 0, mask, color: dib };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(dib);
        DeleteObject(mask);
        icon
    }

    fn wide(s: &str) -> Vec<WCHAR> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
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
        unsafe {
            let mut hk: HKEY = std::ptr::null_mut();
            // Querying a value directly on the HKCU predefined handle returned
            // ERROR_FILE_NOT_FOUND even when the value existed (verified side
            // by side: open-then-query succeeds). Always open the Run key first.
            if RegOpenKeyExW(key, wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run").as_ptr(), 0, KEY_QUERY_VALUE, &mut hk) != 0 {
                return false;
            }
            let mut kind: DWORD = 0;
            let mut size: DWORD = 0;
            let rc = RegQueryValueExW(hk, wide(&name).as_ptr(), std::ptr::null(), &mut kind, std::ptr::null_mut(), &mut size);
            RegCloseKey(hk);
            rc == 0 || rc == 234 // ERROR_SUCCESS / ERROR_MORE_DATA both mean it exists
        }
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

    /// (remaining_secs, requested_secs, screen kept on) of a live awake hold.
    fn awake_state() -> Option<(u64, u64, bool)> {
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
        Some((get("deadline")?.saturating_sub(now), get("requested").unwrap_or(0), get("display") == Some(1)))
    }

    fn fmt_secs(s: u64) -> String {
        let (h, m) = (s / 3600, (s % 3600) / 60);
        if h > 0 { format!("{h}h{m:02}m") } else { format!("{m}m") }
    }

    /// Show a tray balloon. Failures and setting changes must be visible —
    /// silent background threads are how "点了没反应" happens.
    unsafe fn notify(hwnd: HWND, title: &str, body: &str) {
        let mut n = NOTIFYICONDATAW::new(hwnd);
        // Only claim the balloon fields: NIM_MODIFY honors uFlags, and letting
        // NIF_ICON through with a NULL hIcon WIPES the tray image (the
        // "icon shows up then vanishes" bug).
        n.uflags = NIF_INFO;
        n.info_flags = 0x1; // NIIF_INFO
        let t = wide(title);
        let tl = t.len().min(63);
        n.info_title[..tl].copy_from_slice(&t[..tl]);
        let b = wide(body);
        let bl = b.len().min(255);
        n.info[..bl].copy_from_slice(&b[..bl]);
        Shell_NotifyIconW(NIM_MODIFY, &mut n);
    }

    /// Hover tooltip = always-visible state line: screen count, secondary
    /// display on/off, awake hold remaining.
    unsafe fn refresh_tooltip(hwnd: HWND) {
        let displays = monctl::windows::list();
        let active = displays.iter().filter(|d| d.active).count();
        let secondary_on = displays.iter().any(|d| d.active && !d.main);
        let mut tip = format!("monctl · {active} 块屏 · 副屏{}", if secondary_on { "开" } else { "关" });
        if let Some((r, _, display_hold)) = awake_state() {
            tip.push_str(&format!(" · 防休眠 {}{}", fmt_secs(r), if display_hold { "，屏幕常亮" } else { "" }));
        }
        tip.push_str("（点击打开菜单）");
        let mut n = NOTIFYICONDATAW::new(hwnd);
        n.uflags = NIF_TIP; // tip only — never the icon lane (see notify)
        let t = wide(&tip);
        let tl = t.len().min(127);
        n.tip[..tl].copy_from_slice(&t[..tl]);
        Shell_NotifyIconW(NIM_MODIFY, &mut n);
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
            let mut flags = MF_STRING;
            if d.active && d.main {
                // Windows drivers may allow detaching the primary but refuse
                // the re-attach (NVIDIA实测 DISP_CHANGE_FAILED), and the OS
                // has no CCD path left to recover it — the screen stays dark
                // until reboot. Never offer it.
                title.push_str("（主屏不可关闭）");
                flags |= MF_GRAYED;
            }
            if d.active && active_count <= 1 {
                title.push_str(" — 最后一块，不能关");
                flags |= MF_GRAYED;
            }
            AppendMenuW(menu, flags, (ID_DISPLAY_BASE + i as u32) as WPARAM, wide(&title).as_ptr());
        }
        let seen = |id: &str| displays.iter().any(|d| d.id == id);
        let active_names: Vec<&str> = displays.iter().filter(|d| d.active).map(|d| d.name.as_str()).collect();
        for (i, (id, name)) in remember.iter().enumerate() {
            // A remembered id that's gone from the OS while an ACTIVE display
            // carries the same name is the same panel under a new id (ids
            // shift across reboots/hardware changes) — not a second screen.
            // Keep the state entry so 全部开启 can still try it.
            if !seen(id) && !active_names.contains(&name.as_str()) {
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

        let awake = awake_state();
        let display_hold = awake.map(|(_, _, d)| d).unwrap_or(false);
        let kd_flags = MF_STRING | if display_hold { MF_CHECKED } else { 0 };
        AppendMenuW(menu, kd_flags, ID_KEEP_DISPLAY as WPARAM, wide("屏幕常亮").as_ptr());
        let awake_label = match awake {
            Some((r, _, _)) if r > 5 * 365 * 24 * 3600 => {
                format!("保持唤醒（不限时{}）", if display_hold { "，屏幕常亮" } else { "" })
            }
            Some((r, _, _)) => {
                format!("保持唤醒（剩余 {}{}）", fmt_secs(r), if display_hold { "，屏幕常亮" } else { "" })
            }
            None => "保持唤醒".to_string(),
        };
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, wide(&awake_label).as_ptr());
        for (i, (label, secs)) in AWAKE_SECS.iter().enumerate() {
            // tick the duration currently on hold (互斥单选)
            let checked = match awake {
                Some((_, requested, _)) => requested == *secs,
                None => false,
            };
            let flags = MF_STRING | if checked { MF_CHECKED } else { 0 };
            AppendMenuW(menu, flags, (ID_AWAKE_BASE + 10 + i as u32) as WPARAM, wide(label).as_ptr());
        }
        let off_flags = MF_STRING | if awake.is_some() { 0 } else { MF_GRAYED };
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
                let name = remember.get(&key).cloned().unwrap_or_else(|| key.clone());
                let hwnd_addr = hwnd as usize;
                std::thread::spawn(move || {
                    let hwnd = hwnd_addr as HWND;
                    match monctl::windows::set_enabled(&key, true) {
                        Ok(()) => {
                            let mut st = common::load_state();
                            st.remove(&key);
                            common::save_state(&st);
                            unsafe {
                                notify(hwnd, "monctl", &format!("已开启 {name}"));
                                refresh_tooltip(hwnd);
                            }
                        }
                        Err(e) => {
                            // keep the state entry so the display stays recoverable
                            let log = common::state_path().with_file_name("tray-error.log");
                            let _ = std::fs::write(log, format!("{e}\n"));
                            unsafe { notify(hwnd, &format!("开启 {name} 失败"), &format!("{e}\n（重新插拔显示器线、注销重登或重启后可用「全部开启」找回）")); }
                        }
                    }
                });
            }
            return;
        }
        if id >= ID_DISPLAY_BASE {
            let ui = UI.with(|u| u.borrow().clone());
            let idx = (id - ID_DISPLAY_BASE) as usize;
            if let Some(d) = ui.snapshot.get(idx) {
                let d = d.clone();
                let hwnd_addr = hwnd as usize;
                std::thread::spawn(move || {
                    let hwnd = hwnd_addr as HWND;
                    if d.active && monctl::windows::list().iter().filter(|x| x.active).count() <= 1 {
                        return; // last-screen guard (menu item is grayed; belt & suspenders)
                    }
                    let turning_off = d.active;
                    match monctl::windows::set_enabled(&d.id, !d.active) {
                        Ok(()) => {
                            let mut st = common::load_state();
                            if d.active {
                                st.insert(d.id.clone(), d.name.clone());
                            } else {
                                st.remove(&d.id);
                            }
                            common::save_state(&st);
                            let verb = if turning_off { "已关闭" } else { "已开启" };
                            unsafe {
                                notify(hwnd, "monctl", &format!("{verb} {}", d.name));
                                refresh_tooltip(hwnd);
                            }
                        }
                        Err(e) => {
                            let log = common::state_path().with_file_name("tray-error.log");
                            let _ = std::fs::write(log, format!("{e}\n"));
                            let verb = if turning_off { "关闭" } else { "开启" };
                            unsafe {
                                notify(hwnd, &format!("{verb} {} 失败", d.name), &format!("{e}\n（部分驱动不允许此操作，主屏尤其常见）"));
                            }
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
            i if (ID_AWAKE_BASE + 10..=ID_AWAKE_BASE + 13).contains(&i) => {
                let secs = AWAKE_SECS[(i - ID_AWAKE_BASE - 10) as usize].1;
                if secs == 0 {
                    spawn_monctl(&["awake"]);
                } else {
                    spawn_monctl(&["awake", &secs.to_string()]);
                }
                let label = AWAKE_SECS[(i - ID_AWAKE_BASE - 10) as usize].0.to_string();
                unsafe {
                    notify(hwnd, "保持唤醒", &format!("已选择 {label}，防休眠生效中"));
                }
                // awake.json is written by the spawned monctl ~0.7s later;
                // refresh the tooltip once that state is on disk
                let hwnd_addr = hwnd as usize;
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    unsafe { refresh_tooltip(hwnd_addr as HWND) };
                });
            }
            i if i == ID_AWAKE_BASE + 14 => {
                spawn_monctl(&["awake", "off"]);
                unsafe {
                    notify(hwnd, "保持唤醒", "已解除");
                }
                let hwnd_addr = hwnd as usize;
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    unsafe { refresh_tooltip(hwnd_addr as HWND) };
                });
            }
            ID_KEEP_DISPLAY => {
                // one-click screen-sleep prevention: unlimited hold with --display
                let turning_off = awake_state().map(|(_, _, d)| d).unwrap_or(false);
                if turning_off {
                    spawn_monctl(&["awake", "off"]);
                } else {
                    spawn_monctl(&["awake", "--display"]);
                }
                unsafe {
                    notify(hwnd, "屏幕常亮", if turning_off { "已关闭，屏幕可正常休眠" } else { "已开启：屏幕不再休眠，直到再次点击" });
                }
                let hwnd_addr = hwnd as usize;
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    unsafe { refresh_tooltip(hwnd_addr as HWND) };
                });
            }
            ID_AUTOSTART => {
                let on = !autostart_enabled();
                set_autostart(on);
                unsafe {
                    if on {
                        notify(hwnd, "monctl 开机启动", &format!("已开启：登录后自动运行 {}", exe_path().display()));
                    } else {
                        notify(hwnd, "monctl 开机启动", "已关闭");
                    }
                }
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
            add_icon_with_retry(hwnd);
            return 0;
        }
        if msg == WM_TIMER && wp == RETRY_TIMER_ID {
            if add_icon(hwnd) {
                KillTimer(hwnd, RETRY_TIMER_ID);
            }
            return 0;
        }
        if msg == WM_TIMER && wp == REFRESH_TIMER_ID {
            KillTimer(hwnd, REFRESH_TIMER_ID);
            // Boot race, part two: even a "successful" NIM_ADD during explorer
            // startup can land in a shell that never binds it — the icon exists
            // in the data layer but the taskbar never draws it. A delete +
            // re-add once the desktop is fully up forces the bind.
            let mut n = NOTIFYICONDATAW::new(hwnd);
            Shell_NotifyIconW(NIM_DELETE, &mut n);
            add_icon_with_retry(hwnd);
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

    /// (Re-)register the tray icon. Returns whether the shell accepted it.
    unsafe fn add_icon(hwnd: HWND) -> bool {
        let mut n = NOTIFYICONDATAW::new(hwnd);
        n.icon = make_icon();
        let tip = wide("monctl");
        let tlen = tip.len().min(127);
        n.tip[..tlen].copy_from_slice(&tip[..tlen]);
        Shell_NotifyIconW(NIM_ADD, &mut n) != 0
    }

    const RETRY_TIMER_ID: usize = 1;
    const REFRESH_TIMER_ID: usize = 2;

    /// The taskbar broadcasts TaskbarCreated very early during explorer
    /// startup, sometimes before its tray is ready to accept icons — a
    /// fire-and-forget NIM_ADD is silently dropped and the icon never
    /// appears (the classic "autostart tray missing" race). Keep retrying
    /// every 500 ms until the shell takes it.
    unsafe fn add_icon_with_retry(hwnd: HWND) {
        if add_icon(hwnd) {
            return;
        }
        SetTimer(hwnd, RETRY_TIMER_ID, 500, std::ptr::null_mut());
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
            if !add_icon(hwnd) {
                // most common at logon: the tray autostarts while explorer is
                // still booting. Retry until the shell accepts the icon
                // instead of giving up (and "losing" the tray icon).
                SetTimer(hwnd, RETRY_TIMER_ID, 500, std::ptr::null_mut());
            }
            // And even an accepted NIM_ADD during boot may end up unbound (the
            // taskbar never draws it). Re-register once the desktop settles.
            SetTimer(hwnd, REFRESH_TIMER_ID, 12_000, std::ptr::null_mut());
            // one-shot balloon so people can locate the icon (often hidden in the ^ overflow)
            let mut b = NOTIFYICONDATAW::new(hwnd);
            b.uflags = NIF_INFO; // balloon only — never the icon lane
            b.info_flags = 0x1; // NIIF_INFO
            let title = wide("monctl 托盘已启动");
            let t = title.len().min(63);
            b.info_title[..t].copy_from_slice(&title[..t]);
            let body = wide("点击图标打开菜单，管理屏幕和防休眠；Ctrl+Alt+Shift+D 恢复全部显示器。");
            let bl = body.len().min(255);
            b.info[..bl].copy_from_slice(&body[..bl]);
            Shell_NotifyIconW(NIM_MODIFY, &mut b);
            UI.with(|u| u.borrow_mut().hwnd = hwnd);
            RegisterHotKey(hwnd, 1, HOTKEY_MODS, HOTKEY_VK);
            refresh_tooltip(hwnd);

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
