use thiserror::Error;

#[derive(Debug, Error)]
pub enum HandsError {
    #[error("failed to set per-monitor DPI awareness v2: {0}")]
    Dpi(String),
    #[error("virtual-screen measurement failed: {0}")]
    Space(String),
    #[error("screenshot capture failed: {0}")]
    Capture(String),
    #[error("UIA walk failed: {0}")]
    Uia(String),
    #[error("{0}")]
    Observe(String),
}

impl HandsError {
    pub fn tool_message(&self) -> String {
        self.to_string()
    }
}
