//! Explicit, delegated Microsoft metadata connection for strict shared intake.
pub mod audit;
pub mod auth;
pub mod hashing;
pub mod proof;
pub mod transport;
pub use auth::{AuthConfig, DevicePrompt, MicrosoftClient, SignInProgress, TokenStore};
pub use proof::{Account, FolderBinding};
