//! Native Reference service composition; callers retain publication authority.
use crate::{Error, ValidatedPayload};
use oms_reference::{controller::Controller, scope::Protocol};

/// Shares the bootstrap's authenticated image and the real checked operations
/// append service. Constructing this value performs no lifecycle publication.
pub struct ReferenceProducer {
    controller: Controller,
    protocol: Protocol,
}
impl ReferenceProducer {
    pub fn new(payload: &ValidatedPayload) -> Result<Self, Error> {
        let controller = payload.controller()?;
        // This factory selects NativeIdentity and native_checked_append; it
        // never accepts a caller-supplied digest as executing-image proof.
        let protocol =
            Protocol::production_with_controller(controller.clone()).map_err(Error::integrity)?;
        Ok(Self {
            controller,
            protocol,
        })
    }
    pub fn protocol(&self) -> Result<&Protocol, Error> {
        self.controller.check().map_err(Error::integrity)?;
        Ok(&self.protocol)
    }
    pub fn controller(&self) -> Result<Controller, Error> {
        self.controller.check().map_err(Error::integrity)?;
        Ok(self.controller.clone())
    }
}
