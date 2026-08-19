#![cfg_attr(not(windows), allow(unused))]

#[cfg(not(windows))]
compile_error!("hands is Windows-first");

pub mod actuate;
pub mod allows;
pub mod attach;
pub mod bezier;
pub mod capture;
pub mod challenge;
pub mod chrome;
pub mod classify;
pub mod dialogs;
pub mod dotask;
pub mod error;
pub mod extract;
pub mod fence;
pub mod foreground;
pub mod host_doctor;
pub mod input;
pub mod lease;
pub mod logs;
pub mod mcp;
pub mod native_host;
pub mod observe;
pub mod pick;
pub mod session;
pub mod settle;
pub mod space;
pub mod target;
pub mod uia;

pub use actuate::{ActuateEnvelope, ActuateRequest};
pub use error::HandsError;
pub use extract::{Detail, Element, Extract};
pub use observe::{ObserveEnvelope, ObserveRequest, observe, serialize_envelope};
pub use pick::{GroundRequest, PickEnvelope, PickRequest, run_ground, run_pick, serialize_pick};
pub use session::resolve_session_id;
pub use space::{Space, ensure_dpi, virtual_screen};
