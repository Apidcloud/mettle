//! Capability-owned deferred fields. Observation never acquires their content.

use std::fmt;
use std::sync::Arc;

use crate::{CapabilityError, CapabilityFuture, Span, Value};

pub trait DeferredField: Send + Sync {
    fn resolve(&self, span: Span) -> CapabilityFuture<'_>;
    fn snapshot(&self) -> Value;
    fn call(&self, span: Span) -> CapabilityFuture<'_> {
        Box::pin(async move { Err(CapabilityError::new("field is not callable", span)) })
    }
}

#[derive(Clone)]
pub struct Deferred(pub Arc<dyn DeferredField>);

impl fmt::Debug for Deferred {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Deferred(<owned field>)")
    }
}

impl PartialEq for Deferred {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
