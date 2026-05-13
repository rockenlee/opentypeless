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

pub fn create_output(mode: OutputMode, app_name: &str) -> Box<dyn TextOutput> {
    match mode {
        OutputMode::Keyboard => Box::new(keyboard::KeyboardOutput::new()),
        OutputMode::Clipboard => Box::new(clipboard::ClipboardOutput::new(app_name)),
    }
}
