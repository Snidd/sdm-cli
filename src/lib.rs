//! `sdm`: a command-line client for the Akeneo Supplier Data Manager API.
//!
//! The command surface is generated at runtime from the API's OpenAPI spec
//! (see [`spec`]); [`cli`] turns it into clap commands, [`request`] sends the
//! calls and [`output`] prints the API's JSON unchanged. [`scope`] limits what a
//! directory may touch.

pub mod cli;
pub mod credentials;
pub mod describe;
pub mod error;
pub mod output;
pub mod request;
pub mod scope;
pub mod spec;

pub use cli::run;
pub use error::{CliError, Result};
