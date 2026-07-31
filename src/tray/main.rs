#![windows_subsystem = "windows"]
//! jdrgb-tray — a notification-area menu for picking a preset colour.
//!
//! Hand-rolled Win32, no GUI toolkit: a tray icon, a popup menu with a coloured
//! dot per preset, and a worker that shells out to `jdrgb.exe`. The icon is the
//! current colour, so the project needs no icon artwork at all.
//!
//! It is a separate binary from the CLI because a PE has exactly one subsystem
//! and the CLI must stay a console app. See docs/tray-plan.md.

mod draw;
mod worker;

use std::sync::mpsc::Sender;

use draw::Swatch;
use jdrgb::palette::{DEFAULT_PRESET, Last, PRESETS, load_state, preset_for_rgb, swatch_rgb};

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    DeleteObject, HBITMAP, MONITOR_DEFAULTTONEAREST, MonitorFromPoint,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetDpiForWindow,
    GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONIDENTIFIER,
    Shell_NotifyIconGetRect, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ---------------------------------------------------------------------------
// Message and menu identifiers
// ---------------------------------------------------------------------------

/// The tray's callback message.
const WM_TRAY: u32 = WM_APP + 1;
/// A finished job coming back from the worker; lParam owns a `worker::Outcome`.
const WM_APPLY_DONE: u32 = WM_APP + 2;

/// Notification-icon events. `NIN_SELECT`/`NIN_KEYSELECT` are `WM_USER`-based
/// and windows-sys doesn't surface them.
const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;

const ID_STATUS: u32 = 1;
const ID_OFF: u32 = 2;
const ID_REAPPLY: u32 = 3;
const ID_EXIT: u32 = 4;
const ID_TARGET_MB: u32 = 5;
const ID_TARGET_GPU: u32 = 6;
const ID_TARGET_ALL: u32 = 7;
const ID_TARGET_CAPTION: u32 = 8;
/// Preset *i* is `ID_PRESET_BASE + i`. Kept well clear of the fixed ids above.
const ID_PRESET_BASE: u32 = 0x100;

// ---------------------------------------------------------------------------
// Target
// ---------------------------------------------------------------------------

/// Which controller the menu acts on. Tray-local UI state — jdrgb itself takes
/// this as a flag per invocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Mb,
    Gpu,
    All,
}

impl Target {
    fn flag(self) -> Option<&'static str> {
        match self {
            Target::Mb => None, // the CLI's default
            Target::Gpu => Some("--gpu"),
            Target::All => Some("--all"),
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Target::Mb => "mb",
            Target::Gpu => "gpu",
            Target::All => "all",
        }
    }

    fn from_slug(s: &str) -> Option<Target> {
        match s.trim() {
            "mb" => Some(Target::Mb),
            "gpu" => Some(Target::Gpu),
            "all" => Some(Target::All),
            _ => None,
        }
    }

    /// Stored beside the CLI's own state, and for the same reason: it is
    /// per-user, and the install directory is deliberately admin-only writable.
    fn path() -> Option<std::path::PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .map(|p| std::path::PathBuf::from(p).join("jdrgb").join("tray-target"))
    }

    /// Defaults to both devices. The CLI defaults to the strip alone, because
    /// every existing invocation predates the GPU support and must keep
    /// behaving the same — but a menu has no such history, and "set the
    /// lights" means all of them.
    fn load() -> Target {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| Target::from_slug(&s))
            .unwrap_or(Target::All)
    }

    fn save(self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, self.slug());
    }
}

// ---------------------------------------------------------------------------
// What's currently showing
// ---------------------------------------------------------------------------

/// Read fresh from the state file every time it's needed — a terminal
/// `jdrgb red` changes the hardware without telling us, so anything cached
/// goes stale silently.
enum Current {
    Named(&'static str),
    Hex((u8, u8, u8)),
    /// The controller's own off mode. Distinct from the `black` preset, which
    /// is a static #000000 — they look the same but read back differently, and
    /// showing a black disc for `off` was exactly the confusion that made the
    /// state file learn to tell them apart.
    Off,
    /// A per-LED pattern: no single colour to show or reapply.
    Multi,
    /// Never recorded. Notably the state after a boot-time apply, because that
    /// task runs as SYSTEM and writes SYSTEM's copy of the file.
    Unknown,
    /// `--all` where the two devices aren't showing the same preset.
    Mixed,
}

impl Current {
    fn read(target: Target) -> Current {
        let (mb, gpu) = load_state();
        match target {
            Target::Mb => Current::from_slot(mb, false),
            Target::Gpu => Current::from_slot(gpu, true),
            Target::All => {
                let (a, b) = (Current::from_slot(mb, false), Current::from_slot(gpu, true));
                match (&a, &b) {
                    (Current::Named(x), Current::Named(y)) if x == y => a,
                    _ => Current::Mixed,
                }
            }
        }
    }

    fn from_slot(slot: Option<Last>, gpu: bool) -> Current {
        match slot {
            None => Current::Unknown,
            Some(Last::Multi) => Current::Multi,
            Some(Last::Off) => Current::Off,
            Some(Last::Color(rgb)) => match preset_for_rgb(rgb, gpu) {
                Some(name) => Current::Named(name),
                None => Current::Hex(rgb),
            },
        }
    }

    fn swatch(&self) -> Swatch {
        match self {
            Current::Named(name) => Swatch::Solid(swatch_rgb(name)),
            Current::Hex(rgb) => Swatch::Solid(*rgb),
            _ => Swatch::Empty,
        }
    }

    fn label(&self) -> String {
        match self {
            Current::Named(name) => (*name).to_string(),
            Current::Hex((r, g, b)) => format!("#{r:02X}{g:02X}{b:02X}"),
            Current::Off => "off".to_string(),
            Current::Multi => "per-LED pattern".to_string(),
            Current::Unknown => "not set from here".to_string(),
            Current::Mixed => "strip and GPU differ".to_string(),
        }
    }

    /// What `Reapply` should send. Nothing recoverable falls back to the
    /// default preset rather than guessing.
    fn reapply_arg(&self) -> String {
        match self {
            Current::Named(name) => (*name).to_string(),
            Current::Hex((r, g, b)) => format!("{r:02X}{g:02X}{b:02X}"),
            Current::Off => "off".to_string(),
            _ => DEFAULT_PRESET.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// Swatch bitmaps for one DPI. Menus are rebuilt per open, but these outlive
/// them — the colours never change, only the pixel size does.
struct Swatches {
    dpi: u32,
    presets: Vec<HBITMAP>,
    empty: HBITMAP,
}

impl Swatches {
    unsafe fn build(dpi: u32) -> Swatches {
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.max(12);
        let presets = PRESETS
            .iter()
            .map(|&(name, _)| unsafe {
                draw::dib(size, &draw::pixels(size, Swatch::Solid(swatch_rgb(name))))
            })
            .collect();
        let empty = unsafe { draw::dib(size, &draw::pixels(size, Swatch::Empty)) };
        Swatches { dpi, presets, empty }
    }

    unsafe fn destroy(&self) {
        for &b in &self.presets {
            unsafe { DeleteObject(b as _) };
        }
        unsafe { DeleteObject(self.empty as _) };
    }
}

struct App {
    hwnd: HWND,
    nid: NOTIFYICONDATAW,
    icon: HICON,
    icon_swatch: Option<Swatch>,
    taskbar_created: u32,
    target: Target,
    swatches: Option<Swatches>,
    tx: Option<Sender<worker::Job>>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copy a string into a fixed wide buffer, truncating on a char boundary and
/// always leaving room for the terminator.
///
/// `szInfo` is 256 units and `szInfoTitle` 64, and the CLI's stderr is under no
/// obligation to fit — a truncation that split a surrogate pair would render as
/// a replacement glyph in the balloon.
fn set_field(dst: &mut [u16], s: &str) {
    let cap = dst.len().saturating_sub(1);
    let mut n = 0;
    dst.fill(0);
    for ch in s.chars() {
        let w = ch.len_utf16();
        if n + w > cap {
            break;
        }
        ch.encode_utf16(&mut dst[n..n + w]);
        n += w;
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

fn main() {
    unsafe {
        // Before any window or DPI-dependent call, or the first measurement is
        // taken in the wrong units and everything downstream inherits it.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        if already_running() {
            return;
        }
        init_dark_mode();

        let hwnd = create_window();
        if hwnd.is_null() {
            return;
        }

        let mut app = Box::new(App {
            hwnd,
            nid: core::mem::zeroed(),
            icon: core::ptr::null_mut(),
            icon_swatch: None,
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            target: Target::load(),
            swatches: None,
            tx: None,
        });
        app.tx = Some(worker::spawn(hwnd, WM_APPLY_DONE));

        let app = Box::into_raw(app);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);

        add_icon(app);
        refresh_icon(app);

        let mut msg: MSG = core::mem::zeroed();
        while GetMessageW(&mut msg, core::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Orderly teardown: Explorer keeps showing a stale icon otherwise.
        Shell_NotifyIconW(NIM_DELETE, &(*app).nid);
        if !(*app).icon.is_null() {
            DestroyIcon((*app).icon);
        }
        if let Some(s) = &(*app).swatches {
            s.destroy();
        }
        drop(Box::from_raw(app));
    }
}

/// A second launch would stack a duplicate icon rather than doing anything
/// useful. The handle is deliberately leaked: it must live as long as we do.
unsafe fn already_running() -> bool {
    let name = wide("Local\\jdrgb-tray-singleton");
    let h = unsafe { CreateMutexW(core::ptr::null(), 1, name.as_ptr()) };
    h.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS
}

unsafe fn create_window() -> HWND {
    let class = wide("jdrgb_tray");
    let hinst = unsafe { GetModuleHandleW(core::ptr::null()) };

    let wc = WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: core::ptr::null_mut(),
        hCursor: core::ptr::null_mut(),
        hbrBackground: core::ptr::null_mut(),
        lpszMenuName: core::ptr::null(),
        lpszClassName: class.as_ptr(),
    };
    unsafe { RegisterClassW(&wc) };

    // A real top-level window that is simply never shown — *not* HWND_MESSAGE.
    // Message-only windows don't receive broadcasts, and TaskbarCreated is one.
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            hinst,
            core::ptr::null(),
        )
    }
}

/// Windows 11 dark menus, via the undocumented uxtheme ordinals hdd-toggle uses.
///
/// Every lookup is guarded: these are ordinals, not exports, and a future build
/// is free to renumber them. Losing dark menus is a cosmetic regression, so the
/// failure mode is a light menu rather than a crash.
unsafe fn init_dark_mode() {
    let name = wide("uxtheme.dll");
    let lib =
        unsafe { LoadLibraryExW(name.as_ptr(), core::ptr::null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if lib.is_null() {
        return;
    }
    // 135 = SetPreferredAppMode, 136 = FlushMenuThemes.
    if let Some(p) = unsafe { GetProcAddress(lib, 135 as _) } {
        let set: extern "system" fn(i32) -> i32 = unsafe { core::mem::transmute(p) };
        set(1); // AllowDark: follow the system setting
    }
    if let Some(p) = unsafe { GetProcAddress(lib, 136 as _) } {
        let flush: extern "system" fn() = unsafe { core::mem::transmute(p) };
        flush();
    }
}

// ---------------------------------------------------------------------------
// Tray icon
// ---------------------------------------------------------------------------

unsafe fn add_icon(app: *mut App) {
    let nid = unsafe { &mut (*app).nid };
    nid.cbSize = core::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = unsafe { (*app).hwnd };
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = WM_TRAY;
    set_field(&mut nid.szTip, "jdrgb");

    // Explorer's notification area may not be ready yet at logon.
    for attempt in 0..10 {
        if unsafe { Shell_NotifyIconW(NIM_ADD, nid) } != 0 {
            break;
        }
        if attempt == 9 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1000));
    }

    // Must follow *every* successful add, including Explorer-restart recovery:
    // it selects the modern callback packing the WM_TRAY handler assumes.
    nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    unsafe { Shell_NotifyIconW(NIM_SETVERSION, nid) };
}

/// Redraw the icon if the colour it should show has changed.
unsafe fn refresh_icon(app: *mut App) {
    let target = unsafe { (*app).target };
    let current = Current::read(target);
    let swatch = current.swatch();

    if unsafe { (*app).icon_swatch } == Some(swatch) {
        return;
    }

    let dpi = unsafe { icon_dpi(app) };
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.max(12);
    let icon = unsafe { draw::icon(size, &draw::pixels(size, swatch)) };
    if icon.is_null() {
        return;
    }

    let old = unsafe { (*app).icon };
    unsafe {
        (*app).icon = icon;
        (*app).icon_swatch = Some(swatch);
        let nid = &mut (*app).nid;
        nid.uFlags = NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        nid.hIcon = icon;
        set_field(&mut nid.szTip, &format!("jdrgb - {}", current.label()));
        Shell_NotifyIconW(NIM_MODIFY, nid);
        if !old.is_null() {
            DestroyIcon(old);
        }
    }
}

/// The DPI of the monitor the tray icon is actually on.
///
/// Not the hidden window's: it never moves, so on a mixed-DPI setup it can
/// easily report a different monitor than the taskbar sits on.
unsafe fn icon_dpi(app: *mut App) -> u32 {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: core::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: unsafe { (*app).hwnd },
        uID: 1,
        guidItem: unsafe { core::mem::zeroed() },
    };
    let mut rect: RECT = unsafe { core::mem::zeroed() };
    if unsafe { Shell_NotifyIconGetRect(&id, &mut rect) } == 0 {
        return unsafe {
            dpi_for_point(POINT {
                x: (rect.left + rect.right) / 2,
                y: (rect.top + rect.bottom) / 2,
            })
        };
    }
    let dpi = unsafe { GetDpiForWindow((*app).hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

unsafe fn dpi_for_point(pt: POINT) -> u32 {
    let mon = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let (mut dx, mut dy) = (96u32, 96u32);
    if unsafe { GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } == 0 && dx > 0 {
        dx
    } else {
        96
    }
}

unsafe fn balloon(app: *mut App, title: &str, text: &str) {
    unsafe {
        let nid = &mut (*app).nid;
        nid.uFlags = NIF_INFO;
        set_field(&mut nid.szInfoTitle, title);
        set_field(&mut nid.szInfo, text);
        Shell_NotifyIconW(NIM_MODIFY, nid);
    }
}

// ---------------------------------------------------------------------------
// Menu
// ---------------------------------------------------------------------------

unsafe fn ensure_swatches(app: *mut App, dpi: u32) {
    let stale = match unsafe { &(*app).swatches } {
        Some(s) => s.dpi != dpi,
        None => true,
    };
    if !stale {
        return;
    }
    // The old set is only safe to free now: no menu is on screen holding it.
    if let Some(old) = unsafe { (*app).swatches.take() } {
        unsafe { old.destroy() };
    }
    unsafe { (*app).swatches = Some(Swatches::build(dpi)) };
}

unsafe fn add_item(menu: HMENU, id: u32, text: &str, bitmap: Option<HBITMAP>, enabled: bool) {
    let mut label = wide(text);
    let mut mii: MENUITEMINFOW = unsafe { core::mem::zeroed() };
    mii.cbSize = core::mem::size_of::<MENUITEMINFOW>() as u32;
    mii.fMask = MIIM_ID | MIIM_STRING | MIIM_STATE;
    mii.wID = id;
    mii.dwTypeData = label.as_mut_ptr();
    mii.fState = if enabled { MFS_ENABLED } else { MFS_DISABLED };
    if let Some(b) = bitmap {
        mii.fMask |= MIIM_BITMAP;
        mii.hbmpItem = b;
    }
    unsafe { InsertMenuItemW(menu, GetMenuItemCount(menu) as u32, 1, &mii) };
}

unsafe fn show_menu(app: *mut App, x: i32, y: i32) {
    let dpi = unsafe { dpi_for_point(POINT { x, y }) };
    unsafe { ensure_swatches(app, dpi) };

    let target = unsafe { (*app).target };
    let current = Current::read(target);
    let current_name = match &current {
        Current::Named(n) => Some(*n),
        _ => None,
    };

    let menu = unsafe { CreatePopupMenu() };

    // Status line: disabled text, which is also what lets Windows 11 give the
    // menu its modern styling rather than falling back to the classic look.
    let status_bmp = match current_name {
        Some(n) => unsafe {
            let idx = PRESETS.iter().position(|&(p, _)| p == n).unwrap_or(0);
            (*app).swatches.as_ref().map(|s| s.presets[idx])
        },
        None => unsafe { (*app).swatches.as_ref().map(|s| s.empty) },
    };
    unsafe { add_item(menu, ID_STATUS, &current.label(), status_bmp, false) };
    unsafe { AppendMenuW(menu, MF_SEPARATOR, 0, core::ptr::null()) };

    // Colours at the top level rather than behind a submenu. It makes for a
    // long menu, but picking one is the only thing this exists to do, and
    // burying the single action behind an extra hop to save height is a bad
    // trade. If it ever outgrows the screen, MFT_MENUBARBREAK on one item
    // splits the list into columns without moving anything.
    for (i, &(name, _)) in PRESETS.iter().enumerate() {
        let bmp = unsafe { (*app).swatches.as_ref().map(|s| s.presets[i]) };
        unsafe { add_item(menu, ID_PRESET_BASE + i as u32, name, bmp, true) };
    }

    let off_bmp = unsafe { (*app).swatches.as_ref().map(|s| s.empty) };
    unsafe {
        AppendMenuW(menu, MF_SEPARATOR, 0, core::ptr::null());
        add_item(menu, ID_OFF, "Off", off_bmp, true);
        AppendMenuW(menu, MF_SEPARATOR, 0, core::ptr::null());
    }

    // Three options don't earn a submenu either. The disabled caption is what
    // stops "Strip / GPU / Both" reading as three more colours.
    unsafe {
        add_item(menu, ID_TARGET_CAPTION, "Target", None, false);
        add_item(menu, ID_TARGET_MB, "Strip", None, true);
        add_item(menu, ID_TARGET_GPU, "GPU", None, true);
        add_item(menu, ID_TARGET_ALL, "Both", None, true);
        // Radio marks live in the check gutter; these items carry no bitmap, so
        // there is nothing for them to collide with.
        let checked = match target {
            Target::Mb => ID_TARGET_MB,
            Target::Gpu => ID_TARGET_GPU,
            Target::All => ID_TARGET_ALL,
        };
        CheckMenuRadioItem(menu, ID_TARGET_MB, ID_TARGET_ALL, checked, MF_BYCOMMAND);
    }

    unsafe {
        AppendMenuW(menu, MF_SEPARATOR, 0, core::ptr::null());
        add_item(menu, ID_REAPPLY, "Reapply", None, true);
        add_item(menu, ID_EXIT, "Exit", None, true);
    }

    // Mark the current preset by making it the default (bold). Deliberately not
    // MFS_CHECKED: a check mark and an item bitmap can end up competing for the
    // same gutter, and bold says "this is what you have" just as well.
    if let Some(name) = current_name {
        if let Some(i) = PRESETS.iter().position(|&(p, _)| p == name) {
            unsafe { SetMenuDefaultItem(menu, ID_PRESET_BASE + i as u32, 0) };
        }
    }

    // Required for tray menus: without the foreground switch the menu can fail
    // to dismiss, and without the trailing post it misbehaves on the next open.
    let hwnd = unsafe { (*app).hwnd };
    unsafe { SetForegroundWindow(hwnd) };
    let cmd = unsafe {
        TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTALIGN | TPM_BOTTOMALIGN,
            x,
            y,
            0,
            hwnd,
            core::ptr::null(),
        )
    };
    unsafe { PostMessageW(hwnd, WM_NULL, 0, 0) };
    unsafe { DestroyMenu(menu) }; // takes the submenus with it

    if cmd > 0 {
        unsafe { on_command(app, cmd as u32) };
    }
}

unsafe fn on_command(app: *mut App, id: u32) {
    match id {
        ID_EXIT => {
            unsafe { PostMessageW((*app).hwnd, WM_CLOSE, 0, 0) };
        }
        ID_TARGET_MB | ID_TARGET_GPU | ID_TARGET_ALL => {
            let t = match id {
                ID_TARGET_MB => Target::Mb,
                ID_TARGET_GPU => Target::Gpu,
                _ => Target::All,
            };
            unsafe { (*app).target = t };
            t.save();
            // The icon tracks the selected target, so it may mean something
            // different now even though nothing was applied.
            unsafe { refresh_icon(app) };
        }
        ID_OFF => unsafe { apply(app, "off", "off") },
        ID_REAPPLY => {
            let target = unsafe { (*app).target };
            let arg = Current::read(target).reapply_arg();
            unsafe { apply(app, &arg, &arg) };
        }
        _ if id >= ID_PRESET_BASE => {
            let i = (id - ID_PRESET_BASE) as usize;
            if let Some(&(name, _)) = PRESETS.get(i) {
                unsafe { apply(app, name, name) };
            }
        }
        _ => {}
    }
}

unsafe fn apply(app: *mut App, arg: &str, label: &str) {
    let mut args = vec![arg.to_string()];
    if let Some(flag) = unsafe { (*app).target }.flag() {
        args.push(flag.to_string());
    }
    if let Some(tx) = unsafe { &(*app).tx } {
        let _ = tx.send(worker::Job { args, label: label.to_string() });
    }
}

// ---------------------------------------------------------------------------
// Window procedure
// ---------------------------------------------------------------------------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let app = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut App;
    if app.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }

    // Explorer restarted and threw away every icon; put ours back.
    if msg == unsafe { (*app).taskbar_created } {
        unsafe {
            add_icon(app);
            (*app).icon_swatch = None; // force the icon to be re-attached
            refresh_icon(app);
        }
        return 0;
    }

    match msg {
        WM_TRAY => {
            // NOTIFYICON_VERSION_4 packing: the event is in the low word of
            // lParam, the anchor point in wParam. The coordinates are signed —
            // a secondary monitor to the left gives negative x.
            let event = (lp as u32) & 0xFFFF;
            let x = (wp & 0xFFFF) as u16 as i16 as i32;
            let y = ((wp >> 16) & 0xFFFF) as u16 as i16 as i32;
            if matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT) {
                unsafe { show_menu(app, x, y) };
            }
            0
        }
        WM_APPLY_DONE => {
            let outcome = unsafe { Box::from_raw(lp as *mut worker::Outcome) };
            // Re-read state whatever the exit code: `--all` applies and records
            // each device independently before reporting failure, so a non-zero
            // exit can still mean the strip changed.
            unsafe { refresh_icon(app) };
            if !outcome.ok {
                unsafe {
                    balloon(
                        app,
                        &format!("jdrgb: {} failed", outcome.label),
                        &outcome.message,
                    )
                };
            }
            0
        }
        WM_CLOSE => {
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            // Drop the queue so the worker thread can finish.
            unsafe { (*app).tx = None };
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_ENDSESSION => {
            unsafe { Shell_NotifyIconW(NIM_DELETE, &(*app).nid) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_round_trips_through_its_slug() {
        for t in [Target::Mb, Target::Gpu, Target::All] {
            assert_eq!(Target::from_slug(t.slug()), Some(t));
        }
        assert_eq!(Target::from_slug("nonsense"), None);
        // A trailing newline from a hand-edited file must still parse.
        assert_eq!(Target::from_slug("gpu\n"), Some(Target::Gpu));
    }

    #[test]
    fn mb_passes_no_flag_so_the_cli_keeps_its_default() {
        assert_eq!(Target::Mb.flag(), None);
        assert_eq!(Target::Gpu.flag(), Some("--gpu"));
        assert_eq!(Target::All.flag(), Some("--all"));
    }

    #[test]
    fn unknown_state_reapplies_the_default_rather_than_guessing() {
        assert_eq!(Current::Unknown.reapply_arg(), DEFAULT_PRESET);
        assert_eq!(Current::Multi.reapply_arg(), DEFAULT_PRESET);
        assert_eq!(Current::Mixed.reapply_arg(), DEFAULT_PRESET);
    }

    #[test]
    fn a_hex_reapplies_without_its_hash() {
        // The bare form is what the CLI's own help shows, so keep the round
        // trip boring even though `#FF0000` would also be accepted.
        assert_eq!(Current::Hex((0xFF, 0x00, 0x00)).reapply_arg(), "FF0000");
        assert_eq!(Current::Hex((0xFF, 0x00, 0x00)).label(), "#FF0000");
    }

    #[test]
    fn only_a_known_colour_gets_a_filled_swatch() {
        assert!(matches!(Current::Named("red").swatch(), Swatch::Solid(_)));
        assert!(matches!(Current::Hex((1, 2, 3)).swatch(), Swatch::Solid(_)));
        for c in [Current::Off, Current::Multi, Current::Unknown, Current::Mixed] {
            assert!(matches!(c.swatch(), Swatch::Empty), "{} should be hollow", c.label());
        }
    }

    /// `off` used to arrive here as `Color((0,0,0))` and come back out as the
    /// `black` preset, so the tray showed a black disc for a controller that
    /// was in its own off mode. They are different states and must stay so.
    #[test]
    fn off_is_not_the_black_preset() {
        let off = Current::from_slot(Some(Last::Off), false);
        let black = Current::from_slot(Some(Last::Color((0, 0, 0))), false);

        assert!(matches!(off, Current::Off));
        assert!(matches!(black, Current::Named("black")));
        assert_eq!(off.label(), "off");
        assert_eq!(black.label(), "black");
        assert!(matches!(off.swatch(), Swatch::Empty));
        assert!(matches!(black.swatch(), Swatch::Solid(_)));
    }

    /// Reapplying `off` must send `off`, not the black preset — otherwise the
    /// controller ends up in static-black mode instead of back where it was.
    #[test]
    fn off_reapplies_as_off() {
        assert_eq!(Current::Off.reapply_arg(), "off");
        assert_eq!(Current::Named("black").reapply_arg(), "black");
    }

    /// A menu is not the CLI: every existing `jdrgb COLOR` invocation has to
    /// keep meaning "the strip", but there is no such history to preserve here.
    #[test]
    fn the_tray_defaults_to_both_devices() {
        assert_eq!(Target::from_slug("").unwrap_or(Target::All), Target::All);
        assert_eq!(Target::All.flag(), Some("--all"));
    }

    #[test]
    fn a_named_swatch_uses_the_appearance_value_not_the_wire_value() {
        // The whole reason SWATCH exists: coolwhite is #FFB0D0 on the wire and
        // would draw as pink.
        assert!(matches!(
            Current::Named("coolwhite").swatch(),
            Swatch::Solid(rgb) if rgb == swatch_rgb("coolwhite")
        ));
    }

    #[test]
    fn every_preset_can_be_addressed_by_its_menu_id() {
        // The ids are positional, so a preset added at the front must not push
        // another one's id onto a fixed command.
        for (i, &(name, _)) in PRESETS.iter().enumerate() {
            let id = ID_PRESET_BASE + i as u32;
            assert!(id > ID_TARGET_ALL, "{name} collides with a fixed command id");
            assert_eq!(PRESETS[(id - ID_PRESET_BASE) as usize].0, name);
        }
    }

    #[test]
    fn balloon_text_truncates_on_a_char_boundary() {
        let mut buf = [0u16; 8];
        set_field(&mut buf, "abcdefghijklmnop");
        assert_eq!(buf[7], 0, "must stay null-terminated");
        assert_eq!(String::from_utf16(&buf[..7]).unwrap(), "abcdefg");
    }

    #[test]
    fn balloon_text_never_splits_a_surrogate_pair() {
        // An emoji is two UTF-16 units; with one slot left it must be dropped
        // whole rather than half-written as a lone surrogate.
        let mut buf = [0u16; 4];
        set_field(&mut buf, "ab\u{1F600}");
        assert_eq!(&buf[..3], &['a' as u16, 'b' as u16, 0]);
        assert!(String::from_utf16(&buf[..2]).is_ok());
    }

    #[test]
    fn short_text_is_copied_whole() {
        let mut buf = [0u16; 16];
        set_field(&mut buf, "off");
        assert_eq!(String::from_utf16(&buf[..3]).unwrap(), "off");
        assert_eq!(buf[3], 0);
    }
}
