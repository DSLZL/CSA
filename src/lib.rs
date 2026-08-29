pub mod activation;
pub mod cli;
pub mod compat;
pub mod detect;
pub mod error;
pub mod hash;
pub mod i18n;
pub mod isolation;
pub mod manager;
pub mod online;
pub mod process;
pub mod state;

pub const BUILD_TARGET: &str = env!("CSA_BUILD_TARGET");
