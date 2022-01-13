use async_stream::stream;
use async_trait::async_trait;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_dynamodb::error::{
    DeleteItemError, DeleteItemErrorKind, PutItemError, PutItemErrorKind, ScanError, ScanErrorKind,
    UpdateItemError, UpdateItemErrorKind,
};
use aws_sdk_dynamodb::model::{
    AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType,
    ScalarAttributeType,
};
use aws_sdk_dynamodb::{Client, Endpoint, Region, SdkError};
use chrono::{DateTime, SecondsFormat, Utc};
use scheduler::store::backing::{Backing, Error, Result};
use scheduler::ScheduledJob;
use std::pin::Pin;
use tokio_stream::Stream;

const PK_KEY: &str = "scheduled_job_id";
const DATA_KEY: &str = "data";
const LAST_RUN_KEY: &str = "last_run";

/// `DynamoDB` backing store
pub struct Store {
    client: Client,
    table: String,
}

impl Store {
    /// Creates a new backing store with default settings for `DynamoDB`.
    /// If region is set to `localhost` it will be configured to read from a local `DynamoDB` instance
    /// for local development and integration tests and table automatically created.
    ///
    /// # Errors
    ///
    /// Will return `Err` if initializing the `DynamoDB` Client fails or when running locally
    /// fails to create the `DynamoDB` table.
    pub async fn default(region: String, table: String) -> anyhow::Result<Self> {
        let local = region == "localhost";
        let region_provider = RegionProviderChain::default_provider().or_else(Region::new(region));
        let shared_config = aws_config::from_env().region(region_provider).load().await;

        let dynamodb_local_config = if local {
            aws_sdk_dynamodb::config::Builder::from(&shared_config)
                .endpoint_resolver(
                    // 8000 is the default dynamodb port
                    Endpoint::immutable(http::Uri::from_static("http://localhost:8000")),
                )
                .build()
        } else {
            aws_sdk_dynamodb::config::Builder::from(&shared_config).build()
        };

        let client = Client::from_conf(dynamodb_local_config);
        let store = Self { client, table };
        store.create_table().await?;
        Ok(store)
    }

    async fn create_table(&self) -> anyhow::Result<()> {
        if let Err(e) = self
            .client
            .delete_table()
            .table_name(&self.table)
            .send()
            .await
        {
            dbg!(e);
        }

        let pk = KeySchemaElement::builder()
            .attribute_name(PK_KEY)
            .key_type(KeyType::Hash)
            .build();

        let ad_pk = AttributeDefinition::builder()
            .attribute_name(PK_KEY)
            .attribute_type(ScalarAttributeType::S)
            .build();

        self.client
            .create_table()
            .billing_mode(BillingMode::PayPerRequest)
            .table_name(&self.table)
            .key_schema(pk)
            .attribute_definitions(ad_pk)
            .send()
            .await?;

        Ok(())
    }
}

#[async_trait]
impl Backing for Store {
    async fn upsert(&self, job: &ScheduledJob) -> Result<()> {
        let data = serde_json::to_string(&job).map_err(|e| Error::Upsert {
            job_id: job.id.clone(),
            message: e.to_string(),
            is_retryable: false,
        })?;

        self.client
            .put_item()
            .table_name(&self.table)
            .item(PK_KEY, AttributeValue::S(job.id.clone()))
            .item(DATA_KEY, AttributeValue::S(data))
            .send()
            .await
            .map_err(|e| Error::Upsert {
                job_id: job.id.clone(),
                message: e.to_string(),
                is_retryable: is_retryable_put(e),
            })?;

        Ok(())
    }

    async fn touch(&self, job_id: &str, last_run: &DateTime<Utc>) -> Result<()> {
        self.client
            .update_item()
            .table_name(&self.table)
            .key(PK_KEY, AttributeValue::S(job_id.to_string()))
            .expression_attribute_names("#s", LAST_RUN_KEY)
            .expression_attribute_values(
                ":lr",
                AttributeValue::S(last_run.to_rfc3339_opts(SecondsFormat::Nanos, true)),
            )
            .update_expression("SET #s = :lr")
            .send()
            .await
            .map_err(|e| Error::Touch {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable_update(e),
            })?;

        Ok(())
    }

    async fn delete(&self, job_id: &str) -> Result<()> {
        self.client
            .delete_item()
            .table_name(&self.table)
            .key(PK_KEY, AttributeValue::S(job_id.to_string()))
            .send()
            .await
            .map_err(|e| Error::Delete {
                job_id: job_id.to_string(),
                message: e.to_string(),
                is_retryable: is_retryable_delete(e),
            })?;
        Ok(())
    }

    fn recover(&self) -> Pin<Box<dyn Stream<Item = Result<ScheduledJob>> + '_>> {
        Box::pin(stream! {
            let mut last = None;

            loop {
                let results = self
                    .client
                    .scan()
                    .table_name(&self.table)
                    .limit(10_000)
                    .set_exclusive_start_key(last)
                    .send()
                    .await
                    .map_err(|e| Error::Recovery {
                        message: e.to_string(),
                        is_retryable: is_retryable_scan(e),
                    })?;

                if let Some(items) = results.items {
                    for item in items {
                        let data = match item.get(DATA_KEY) {
                            None => None,
                            Some(av) => match av {
                                AttributeValue::S(data) => {
                                    let job: ScheduledJob = serde_json::from_str(data).map_err(|e| Error::Recovery {
                                        message: e.to_string(),
                                        is_retryable: false,
                                    })?;
                                    Some(job)
                                }
                                _ => None,
                            },
                        };
                        if let Some(job) = data {
                            yield Ok(job);
                        }
                    }
                }

                if let Some(last_key) = results.last_evaluated_key {
                    last = Some(last_key);
                    continue;
                }
                break;
            }
        })
    }
}

#[inline]
fn is_retryable_put(e: SdkError<PutItemError>) -> bool {
    match e {
        SdkError::ServiceError { err, .. } => matches!(
            err.kind,
            PutItemErrorKind::ProvisionedThroughputExceededException(_)
                | PutItemErrorKind::RequestLimitExceeded(_)
        ),
        _ => is_retryable(e),
    }
}

#[inline]
fn is_retryable_delete(e: SdkError<DeleteItemError>) -> bool {
    match e {
        SdkError::ServiceError { err, .. } => matches!(
            err.kind,
            DeleteItemErrorKind::ProvisionedThroughputExceededException(_)
                | DeleteItemErrorKind::RequestLimitExceeded(_)
        ),
        _ => is_retryable(e),
    }
}

#[inline]
fn is_retryable_update(e: SdkError<UpdateItemError>) -> bool {
    match e {
        SdkError::ServiceError { err, .. } => matches!(
            err.kind,
            UpdateItemErrorKind::ProvisionedThroughputExceededException(_)
                | UpdateItemErrorKind::RequestLimitExceeded(_)
        ),
        _ => is_retryable(e),
    }
}

#[inline]
fn is_retryable_scan(e: SdkError<ScanError>) -> bool {
    match e {
        SdkError::ServiceError { err, .. } => matches!(
            err.kind,
            ScanErrorKind::ProvisionedThroughputExceededException(_)
                | ScanErrorKind::RequestLimitExceeded(_)
        ),
        _ => is_retryable(e),
    }
}

#[inline]
fn is_retryable<E>(e: SdkError<E>) -> bool {
    match e {
        SdkError::TimeoutError(_) | SdkError::ResponseError { .. } => true,
        SdkError::DispatchFailure(e) => e.is_timeout() || e.is_io(),
        _ => false,
    }
}
