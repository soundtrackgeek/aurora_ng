//! Windows-wide shortcuts, the same keys as Aurora's defaults. They work
//! while another program has focus, and the rating and Love ones always act
//! on the track that is playing, never on the one selected in the window.
//!
//! Windows lets only one program own a key combination, so they cannot work
//! while Aurora (or anything else using the same keys) is running.

/// What a shortcut does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    Next,
    /// 1–5 stars, or 0 to clear the rating.
    Rate(u8),
    ToggleLove,
}

pub enum Event {
    Pressed(Action),
    /// The keys of shortcuts another program already has.
    Unavailable(Vec<&'static str>),
}

const VK_NUMPAD0: u32 = 0x60;

/// Every shortcut: what it does, its keys as shown, and its Windows virtual
/// key, always with Ctrl+Alt.
const BINDINGS: [(Action, &str, u32); 9] = [
    (Action::PlayPause, "Ctrl+Alt+P", b'P' as u32),
    (Action::Next, "Ctrl+Alt+N", b'N' as u32),
    (Action::Rate(0), "Ctrl+Alt+Num 0", VK_NUMPAD0),
    (Action::Rate(1), "Ctrl+Alt+Num 1", VK_NUMPAD0 + 1),
    (Action::Rate(2), "Ctrl+Alt+Num 2", VK_NUMPAD0 + 2),
    (Action::Rate(3), "Ctrl+Alt+Num 3", VK_NUMPAD0 + 3),
    (Action::Rate(4), "Ctrl+Alt+Num 4", VK_NUMPAD0 + 4),
    (Action::Rate(5), "Ctrl+Alt+Num 5", VK_NUMPAD0 + 5),
    (Action::ToggleLove, "Ctrl+Alt+L", b'L' as u32),
];

/// Registers the shortcuts on a thread of their own and reports what
/// happens. They stay registered until the app exits.
pub fn start() -> async_channel::Receiver<Event> {
    let (sender, receiver) = async_channel::unbounded();
    #[cfg(windows)]
    std::thread::Builder::new()
        .name("global-shortcuts".to_owned())
        .spawn(move || windows::run(&sender))
        .ok();
    #[cfg(not(windows))]
    drop(sender);
    receiver
}

#[cfg(windows)]
mod windows {
    use super::{BINDINGS, Event};
    use std::ffi::c_void;

    const MOD_ALT: u32 = 0x1;
    const MOD_CONTROL: u32 = 0x2;
    /// Holding the keys down does not repeat the action.
    const MOD_NOREPEAT: u32 = 0x4000;
    const WM_HOTKEY: u32 = 0x0312;

    #[repr(C)]
    struct Msg {
        hwnd: *mut c_void,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
        private: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterHotKey(hwnd: *mut c_void, id: i32, modifiers: u32, vk: u32) -> i32;
        fn GetMessageW(msg: *mut Msg, hwnd: *mut c_void, min: u32, max: u32) -> i32;
    }

    /// Without a window, Windows posts the presses to this thread's queue.
    pub(super) fn run(sender: &async_channel::Sender<Event>) {
        let mut unavailable = Vec::new();
        for (id, (_, keys, vk)) in BINDINGS.iter().enumerate() {
            // SAFETY: plain values; a null window means this thread.
            let ok = unsafe {
                RegisterHotKey(
                    std::ptr::null_mut(),
                    id as i32,
                    MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                    *vk,
                )
            };
            if ok == 0 {
                unavailable.push(*keys);
            }
        }
        let none_registered = unavailable.len() == BINDINGS.len();
        if !unavailable.is_empty()
            && sender
                .send_blocking(Event::Unavailable(unavailable))
                .is_err()
            || none_registered
        {
            return;
        }
        let mut msg = Msg {
            hwnd: std::ptr::null_mut(),
            message: 0,
            wparam: 0,
            lparam: 0,
            time: 0,
            pt_x: 0,
            pt_y: 0,
            private: 0,
        };
        // SAFETY: `msg` is a valid MSG for the call to fill in. GetMessageW
        // returns 0 on WM_QUIT and -1 on error; both end the loop.
        while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
            if msg.message != WM_HOTKEY {
                continue;
            }
            let Some((action, _, _)) = BINDINGS.get(msg.wparam) else {
                continue;
            };
            if sender.send_blocking(Event::Pressed(*action)).is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_auroras_defaults() {
        let keys: Vec<_> = BINDINGS.iter().map(|(_, keys, _)| *keys).collect();
        assert_eq!(
            keys,
            [
                "Ctrl+Alt+P",
                "Ctrl+Alt+N",
                "Ctrl+Alt+Num 0",
                "Ctrl+Alt+Num 1",
                "Ctrl+Alt+Num 2",
                "Ctrl+Alt+Num 3",
                "Ctrl+Alt+Num 4",
                "Ctrl+Alt+Num 5",
                "Ctrl+Alt+L",
            ]
        );
        assert_eq!(BINDINGS[2].2, 0x60);
        assert_eq!(BINDINGS[7].2, 0x65);
        assert_eq!(BINDINGS[8].0, Action::ToggleLove);
    }
}
