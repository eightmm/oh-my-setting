//! Canonical task assignment validation, with complete reviewed overrides and
//! no cross-provider model inheritance. This is the shared task producer API.
pub use oms_command_task::assignment::{
    fields, resolve, validate, validate_collaboration, validate_collaboration_plan,
};
