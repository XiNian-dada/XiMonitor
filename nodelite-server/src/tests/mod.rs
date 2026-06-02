//! Library-unit test module wiring.
//!
//! This module starts as a shim around the legacy `lib_tests.rs` file so the
//! suite can be split incrementally without dropping coverage between commits.

pub(crate) use super::{
    AppState, PROTECTED_CACHE_CONTROL, ServerReadiness, set_protected_response_headers,
    uses_insecure_remote_public_base_url,
};

#[path = "../lib_tests.rs"]
mod legacy;

mod auth_runtime_tests;
mod protected_headers_tests;
mod readonly_auth_tests;
mod route_surface_tests;
mod support;
