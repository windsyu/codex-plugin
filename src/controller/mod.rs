//! V2 command and live-source control plane.
//!
//! Browser requests are translated into fixed, typed operations before they
//! enter the actor. No generic JSON-RPC passthrough is exposed.

mod actor;
mod protocol;

pub(crate) use crate::domain::gateway::PendingRequestAction;
pub(crate) use actor::{
    ActorCommand, ActorRequest, CapabilityCatalog, ControllerOperation, ControllerRegistry,
    RegistryError, ReviewTarget, SlashCommandEntry, SourceActorSnapshot, SourceControlCatalog,
    ThreadSetting, actor_channel,
};
