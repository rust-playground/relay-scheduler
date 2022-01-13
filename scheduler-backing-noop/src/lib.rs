use async_trait::async_trait;
use chrono::{DateTime, Utc};
use scheduler::store::backing::{Backing, Result};
use scheduler::ScheduledJob;
use std::pin::Pin;
use tokio_stream::Stream;

/// Noop store which is a placeholder.
#[derive(Default)]
pub struct Store;

#[async_trait]
impl Backing for Store {
    async fn upsert(&self, _job: &ScheduledJob) -> Result<()> {
        Ok(())
    }

    async fn touch(&self, _job_id: &str, _last_run: &DateTime<Utc>) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _job_id: &str) -> Result<()> {
        Ok(())
    }

    #[inline]
    fn recover(&self) -> Pin<Box<dyn Stream<Item = Result<ScheduledJob>> + '_>> {
        Box::pin(tokio_stream::empty())
    }
}
