pub mod clipboard;
pub mod keyboard;

use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputMode {
    Keyboard,
    Clipboard,
}

#[async_trait]
pub trait TextOutput: Send + Sync {
    async fn type_text(&self, text: &str) -> Result<()>;
    fn mode(&self) -> OutputMode;
}

/// Cancel any active IME (Input Method Editor) composition in the foreground
/// window before simulating keyboard input or clipboard paste. Without this,
/// `SendInput`-based typing (enigo) and `Ctrl+V` simulation conflict with the
/// IME composition state, causing crashes or garbled CJK text on Windows.
#[cfg(target_os = "windows")]
pub(crate) fn dismiss_ime_composition() {
    unsafe {
        let hwnd = windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
        if hwnd.is_null() {
            return;
        }
        let himc = windows_sys::Win32::UI::Input::Ime::ImmGetContext(hwnd);
        if himc.is_null() {
            return;
        }
        const NI_COMPOSITIONSTR: u32 = 0x0015;
        const CPS_CANCEL: u32 = 0x0004;
        windows_sys::Win32::UI::Input::Ime::ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
        windows_sys::Win32::UI::Input::Ime::ImmReleaseContext(hwnd, himc);
    }
}

pub fn create_output(mode: OutputMode, app_name: &str) -> Box<dyn TextOutput> {
    match mode {
        OutputMode::Keyboard => Box::new(keyboard::KeyboardOutput::new()),
        OutputMode::Clipboard => Box::new(clipboard::ClipboardOutput::new(app_name)),
    }
}
