use thiserror::Error;
use windows::core::Error as WindowsError;

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
    #[error("{0}")]
    Target(String),
    #[error("{0}")]
    Input(String),
    #[error("{0}")]
    Lease(String),
    #[error("{0}")]
    Fence(String),
    #[error("{0}")]
    Settle(String),
    #[error("{0}")]
    Logs(String),
    #[error("{0}")]
    Chrome(String),
    #[error("{0}")]
    Gemma(String),
    #[error("{0}")]
    Challenge(String),
    #[error("{0}")]
    Listen(String),
    #[error("{0}")]
    DoTask(String),
    #[error("Windows API error: {0}")]
    Windows(String),
}

impl From<WindowsError> for HandsError {
    fn from(e: WindowsError) -> Self {
        HandsError::Windows(e.to_string())
    }
}

impl HandsError {
    pub fn tool_message(&self) -> String {
        self.to_string()
    }
}
