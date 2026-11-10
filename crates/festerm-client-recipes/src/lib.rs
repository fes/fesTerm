#![forbid(unsafe_code)]

//! Public client-local metadata, not host authorization or a connection executor.
//! No file, network, vault, process or UI dependencies belong in this crate.

mod browsing;
mod model;

pub use browsing::{BrowseRequest, BrowseScope, BrowseTicket, HostBrowser};
pub use festerm_host_protocol::{GenerationId, HostId, Lifetime, LogicalSessionId, Recipe};
pub use model::{
    AccountReference, ConnectionBehavior, DuplicatePolicy, HostReference, MetadataCommand,
    MetadataOutcome, PublicRecipe, RecipeBook, RecipeError, RecipeId, MAX_PORTABLE_BYTES,
    MAX_RECIPES,
};
