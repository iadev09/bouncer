use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::core::{Database, Spool};

#[derive(Clone)]
pub struct AppState {
    pub spool: Arc<Spool>,
    pub db: Arc<Database>,
    pub shutdown: CancellationToken,
}
