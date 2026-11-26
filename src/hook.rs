use crate::window_ops::{get_root_window_at, is_excluded_app, isize_to_hwnd};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicIsize, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock, RwLock};
use std::thread::{self, JoinHandle};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU,
    VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PostMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG,
    MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

pub const WM_ZONESNAP_EVENT: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 10;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookMode {
    Idle = 0,
    MaybeStandardDrag = 1,
    StandardDragging = 2,
    AltMoving = 3,
    AltResizing = 4,
    OverlayFromDrag = 5,
    OverlayFromHotkey = 6,
}

impl HookMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::MaybeStandardDrag,
            2 => Self::StandardDragging,
            3 => Self::AltMoving,
            4 => Self::AltResizing,
            5 => Self::OverlayFromDrag,
            6 => Self::OverlayFromHotkey,
            _ => Self::Idle,
        }
    }

    pub fn is_overlay_active(self) -> bool {
        matches!(self, Self::OverlayFromDrag | Self::OverlayFromHotkey)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HookEvent {
    LeftDownCandidate { x: i32, y: i32 },
    StartAltMove { x: i32, y: i32 },
    StartAltResize { x: i32, y: i32 },
    TriggerOverlayFromDrag { x: i32, y: i32 },
    CancelOverlay,
    OverlayHotkeyConfirm { x: i32, y: i32 },
    WheelCycle { delta: i16 },
    LeftUp { x: i32, y: i32 },
    RightUp { x: i32, y: i32 },
    CtrlChanged(bool),
    ShiftChanged(bool),
}

pub struct HookSharedState {
    mode: AtomicU8,
    pub move_mod_vk: AtomicI32,
    pub lock_mod_vk: AtomicI32,
    daemon_hwnd: AtomicIsize,
    pub overlay_hwnd: AtomicIsize,
    pending_suppress_lbutton_up: AtomicBool,
    pending_suppress_rbutton_up: AtomicBool,
    latest_move_packed: AtomicI64,
    move_dirty: AtomicBool,
    last_ctrl_down: AtomicBool,
    last_shift_down: AtomicBool,
    pub excluded_apps: RwLock<HashSet<String>>,
}

impl HookSharedState {
    pub fn new(daemon_hwnd: isize, excluded: HashSet<String>) -> Self {
        Self {
            mode: AtomicU8::new(HookMode::Idle as u8),
            move_mod_vk: AtomicI32::new(VK_MENU.0 as i32),
            lock_mod_vk: AtomicI32::new(VK_CONTROL.0 as i32),
            daemon_hwnd: AtomicIsize::new(daemon_hwnd),
            overlay_hwnd: AtomicIsize::new(0),
            pending_suppress_lbutton_up: AtomicBool::new(false),
            pending_suppress_rbutton_up: AtomicBool::new(false),
            latest_move_packed: AtomicI64::new(0),
            move_dirty: AtomicBool::new(false),
            last_ctrl_down: AtomicBool::new(false),
            last_shift_down: AtomicBool::new(false),
            excluded_apps: RwLock::new(excluded),
        }
    }

    pub fn mode(&self) -> HookMode {
        HookMode::from_u8(self.mode.load(Ordering::Acquire))
    }

    pub fn set_mode(&self, mode: HookMode) {
        self.mode.store(mode as u8, Ordering::Release);
        if mode == HookMode::Idle {
            self.move_dirty.store(false, Ordering::Release);
            self.last_ctrl_down.store(false, Ordering::Release);
            self.last_shift_down.store(false, Ordering::Release);
        }
    }

    pub fn take_latest_move(&self) -> Option<(i32, i32)> {
        if self.move_dirty.swap(false, Ordering::AcqRel) {
            let packed = self.latest_move_packed.load(Ordering::Acquire);
            Some(unpack_point(packed))
        } else {
            None
        }
    }

    pub fn set_excluded_apps(&self, apps: HashSet<String>) {
        if let Ok(mut guard) = self.excluded_apps.write() {
            *guard = apps;
        }
    }
}

#[inline]
fn pack_point(x: i32, y: i32) -> i64 {
    ((x as i64) << 32) | ((y as u32) as i64)
}

#[inline]
fn unpack_point(packed: i64) -> (i32, i32) {
    ((packed >> 32) as i32, (packed & 0xFFFF_FFFF) as i32)
}

fn is_vk_down(vk: i32) -> bool {
    unsafe {
        if vk == VK_LWIN.0 as i32 || vk == VK_RWIN.0 as i32 {
            ((GetAsyncKeyState(VK_LWIN.0 as i32) as u16) & 0x8000 != 0)
                || ((GetAsyncKeyState(VK_RWIN.0 as i32) as u16) & 0x8000 != 0)
        } else {
            ((GetAsyncKeyState(vk) as u16) & 0x8000) != 0
        }
    }
}

fn is_shift_down() -> bool {
    is_vk_down(VK_SHIFT.0 as i32)
}

fn is_point_excluded(state: &HookSharedState, x: i32, y: i32) -> bool {
    let Some(hwnd) = get_root_window_at(x, y) else {
        return false;
    };
    if let Ok(guard) = state.excluded_apps.read() {
        is_excluded_app(hwnd, &guard)
    } else {
        false
    }
}

struct HookContext {
    state: Arc<HookSharedState>,
    tx: Sender<HookEvent>,
}

static HOOK_CTX: OnceLock<HookContext> = OnceLock::new();

fn notify_daemon(state: &HookSharedState) {
    let raw = state.daemon_hwnd.load(Ordering::Acquire);
    if raw != 0 {
        unsafe {
            let _ = PostMessageW(
                isize_to_hwnd(raw),
                WM_ZONESNAP_EVENT,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

fn send_event(ctx: &HookContext, ev: HookEvent) {
    let _ = ctx.tx.send(ev);
    notify_daemon(&ctx.state);
}

unsafe extern "system" fn mouse_hook_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode < 0 {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    }

    let Some(ctx) = HOOK_CTX.get() else {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    };

    let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
    const LLMHF_INJECTED: u32 = 0x0000_0001;
    if (info.flags & LLMHF_INJECTED) != 0 {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let x = info.pt.x;
    let y = info.pt.y;
    let mode = ctx.state.mode();

    match msg {
        WM_MOUSEMOVE => {
            if mode != HookMode::Idle {
                ctx.state
                    .latest_move_packed
                    .store(pack_point(x, y), Ordering::Release);
                if !ctx.state.move_dirty.swap(true, Ordering::AcqRel) {
                    notify_daemon(&ctx.state);
                }
            }
        }
        WM_LBUTTONDOWN => {
            if mode == HookMode::OverlayFromHotkey {
                ctx.state
                    .pending_suppress_lbutton_up
                    .store(true, Ordering::Release);
                send_event(ctx, HookEvent::OverlayHotkeyConfirm { x, y });
                return LRESULT(1);
            }
            if mode == HookMode::Idle {
                if !is_point_excluded(&ctx.state, x, y) {
                    let move_vk = ctx.state.move_mod_vk.load(Ordering::Acquire);
                    if is_vk_down(move_vk) {
                        ctx.state
                            .pending_suppress_lbutton_up
                            .store(true, Ordering::Release);
                        ctx.state.set_mode(HookMode::AltMoving);
                        send_event(ctx, HookEvent::StartAltMove { x, y });
                        return LRESULT(1);
                    } else {
                        ctx.state.set_mode(HookMode::MaybeStandardDrag);
                        send_event(ctx, HookEvent::LeftDownCandidate { x, y });
                    }
                }
            }
        }
        WM_LBUTTONUP => {
            let suppress = ctx
                .state
                .pending_suppress_lbutton_up
                .swap(false, Ordering::AcqRel);
            if mode != HookMode::Idle {
                send_event(ctx, HookEvent::LeftUp { x, y });
            }
            if suppress {
                return LRESULT(1);
            }
        }
        WM_RBUTTONDOWN => {
            match mode {
                HookMode::StandardDragging | HookMode::AltMoving => {
                    ctx.state
                        .pending_suppress_rbutton_up
                        .store(true, Ordering::Release);
                    ctx.state.set_mode(HookMode::OverlayFromDrag);
                    send_event(ctx, HookEvent::TriggerOverlayFromDrag { x, y });
                    return LRESULT(1);
                }
                HookMode::OverlayFromDrag | HookMode::OverlayFromHotkey => {
                    ctx.state
                        .pending_suppress_rbutton_up
                        .store(true, Ordering::Release);
                    send_event(ctx, HookEvent::CancelOverlay);
                    return LRESULT(1);
                }
                HookMode::Idle => {
                    if !is_point_excluded(&ctx.state, x, y) {
                        let move_vk = ctx.state.move_mod_vk.load(Ordering::Acquire);
                        if is_vk_down(move_vk) {
                            ctx.state
                                .pending_suppress_rbutton_up
                                .store(true, Ordering::Release);
                            ctx.state.set_mode(HookMode::AltResizing);
                            send_event(ctx, HookEvent::StartAltResize { x, y });
                            return LRESULT(1);
                        }
                    }
                }
                _ => {}
            }
        }
        WM_RBUTTONUP => {
            let suppress = ctx
                .state
                .pending_suppress_rbutton_up
                .swap(false, Ordering::AcqRel);
            if mode == HookMode::AltResizing {
                send_event(ctx, HookEvent::RightUp { x, y });
            }
            if suppress {
                return LRESULT(1);
            }
        }
        WM_MOUSEWHEEL => {
            if mode.is_overlay_active() {
                let delta = ((info.mouseData >> 16) as u16) as i16;
                if delta != 0 {
                    send_event(ctx, HookEvent::WheelCycle { delta });
                }
                return LRESULT(1);
            }
        }
        _ => {}
    }

    CallNextHookEx(HHOOK::default(), ncode, wparam, lparam)
}

fn vk_matches_modifier(vk: u32, mod_vk: i32) -> bool {
    if mod_vk == VK_CONTROL.0 as i32 {
        vk == VK_CONTROL.0 as u32 || vk == VK_LCONTROL.0 as u32 || vk == VK_RCONTROL.0 as u32
    } else if mod_vk == VK_MENU.0 as i32 {
        vk == VK_MENU.0 as u32 || vk == VK_LMENU.0 as u32 || vk == VK_RMENU.0 as u32
    } else if mod_vk == VK_SHIFT.0 as i32 {
        vk == VK_SHIFT.0 as u32 || vk == VK_LSHIFT.0 as u32 || vk == VK_RSHIFT.0 as u32
    } else if mod_vk == VK_LWIN.0 as i32 || mod_vk == VK_RWIN.0 as i32 {
        vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32
    } else {
        vk == mod_vk as u32
    }
}

unsafe extern "system" fn keyboard_hook_proc(
    ncode: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if ncode < 0 {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    }

    let Some(ctx) = HOOK_CTX.get() else {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    };

    let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
    const LLKHF_INJECTED: u32 = 0x0000_0010;
    if (info.flags.0 & LLKHF_INJECTED) != 0 {
        return CallNextHookEx(HHOOK::default(), ncode, wparam, lparam);
    }

    let msg = wparam.0 as u32;
    let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
    let vk = info.vkCode;
    let mode = ctx.state.mode();

    if mode.is_overlay_active() && is_down && vk == VK_ESCAPE.0 as u32 {
        send_event(ctx, HookEvent::CancelOverlay);
        return LRESULT(1);
    }

    if mode != HookMode::Idle && (is_down || is_up) {
        let lock_vk = ctx.state.lock_mod_vk.load(Ordering::Acquire);
        if vk_matches_modifier(vk, lock_vk) {
            let prev = ctx.state.last_ctrl_down.swap(is_down, Ordering::AcqRel);
            if prev != is_down {
                send_event(ctx, HookEvent::CtrlChanged(is_down));
            }
        }
        if vk_matches_modifier(vk, VK_SHIFT.0 as i32) {
            let down_now = if is_down { true } else { is_shift_down() };
            let prev = ctx.state.last_shift_down.swap(down_now, Ordering::AcqRel);
            if prev != down_now {
                send_event(ctx, HookEvent::ShiftChanged(down_now));
            }
        }
    }

    CallNextHookEx(HHOOK::default(), ncode, wparam, lparam)
}

pub struct HookThread {
    thread_id: u32,
    handle: Option<JoinHandle<()>>,
}

impl HookThread {
    pub fn spawn(state: Arc<HookSharedState>) -> (Self, Receiver<HookEvent>) {
        let (tx, rx) = mpsc::channel();
        let _ = HOOK_CTX.set(HookContext {
            state: Arc::clone(&state),
            tx,
        });

        let (tid_tx, tid_rx) = mpsc::channel::<u32>();
        let handle = thread::Builder::new()
            .name("zonesnap-hook".to_string())
            .spawn(move || unsafe {
                let tid = GetCurrentThreadId();
                let _ = tid_tx.send(tid);

                let mouse_hook = SetWindowsHookExW(
                    WH_MOUSE_LL,
                    Some(mouse_hook_proc),
                    HMODULE::default(),
                    0,
                )
                .ok();
                let kbd_hook = SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(keyboard_hook_proc),
                    HMODULE::default(),
                    0,
                )
                .ok();

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                if let Some(h) = mouse_hook {
                    let _ = UnhookWindowsHookEx(h);
                }
                if let Some(h) = kbd_hook {
                    let _ = UnhookWindowsHookEx(h);
                }
            })
            .expect("failed to spawn hook thread");

        let thread_id = tid_rx.recv().unwrap_or(0);
        (
            Self {
                thread_id,
                handle: Some(handle),
            },
            rx,
        )
    }
}

impl Drop for HookThread {
    fn drop(&mut self) {
        if self.thread_id != 0 {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
