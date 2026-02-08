//! Re-export auto-generated Sails types from IDL.
//!
//! These types are generated from `vara_perps.idl` via `sails-client-gen`.
//!
//! The generated types use `sails_rs::U256` which comes from `gprimitives` →
//! `primitive-types 0.12.2`.  Our direct dep is also 0.12.2, so they are
//! the **same** `U256`.
//!
//! `perp-futures` however depends on `primitive-types 0.14.0` — a **different**
//! crate instance.  Where we pass values from `perp-futures` types into our
//! code we convert via SCALE codec (both versions implement Encode/Decode
//! with identical byte layout).

pub type ActorId = sails_rs::ActorId;

pub use crate::vara::generated::*;

use primitive_types::U256;

// Identity — kept only to avoid touching every call-site in exchange_agent.
#[inline(always)]
pub fn u256_to_sails(value: U256) -> U256 {
    value
}

#[inline(always)]
pub fn u256_from_sails(value: U256) -> U256 {
    value
}

