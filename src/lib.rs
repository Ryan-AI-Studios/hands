#![cfg_attr(not(windows), allow(unused))]

#[cfg(not(windows))]
compile_error!("hands is Windows-first");

pub mod capture;
pub mod error;
pub mod extract;
pub mod mcp;
pub mod observe;
pub mod session;
pub mod space;
pub mod uia;

pub use error::HandsError;
pub use extract::{Detail, Element, Extract};
pub use observe::{ObserveEnvelope, ObserveRequest, observe, serialize_envelope};
pub use session::resolve_session_id;
pub use space::{Space, ensure_dpi, virtual_screen};
