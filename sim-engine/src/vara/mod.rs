pub mod client;
pub mod keystore;
pub mod types;

pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/vara_perps_client.rs"));
}

pub use client::{TxResult, TxType, VaraClient, VaraConfig, VaraError};
pub use keystore::KeystoreManager;
pub use generated::*;
pub use types::{ActorId, u256_from_sails, u256_to_sails};
