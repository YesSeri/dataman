use std::{
    collections::HashSet,
    ops::{Deref, DerefMut},
    sync::Arc,
};

/// History, requests, and rendering share selections until they actually change.
#[derive(Debug, Clone, Default)]
pub(crate) struct Selection(Arc<HashSet<i64>>);
impl Deref for Selection {
    type Target = HashSet<i64>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for Selection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}
