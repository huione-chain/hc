// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use mysten_metrics::init_metrics;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use simulacrum::Simulacrum;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use sui_json_rpc_types::SuiTransactionBlockResponse;

use crate::{
    config::{IngestionConfig, PruningOptions, SnapshotLagConfig, UploadOptions},
    database::{Connection, ConnectionPool},
    db::ConnectionPoolConfig,
    errors::IndexerError,
    indexer::Indexer,
    store::PgIndexerStore,
    tempdb::{get_available_port, TempDb},
    IndexerMetrics,
};

pub enum ReaderWriterConfig {
    Reader { reader_mode_rpc_url: String },
    Writer { snapshot_config: SnapshotLagConfig, pruning_options: PruningOptions },
}

impl ReaderWriterConfig {
    pub fn reader_mode(reader_mode_rpc_url: String) -> Self {
        Self::Reader { reader_mode_rpc_url }
    }

    /// Instantiates a config for indexer in writer mode with the given snapshot config and epochs
    /// to keep.
    pub fn writer_mode(snapshot_config: Option<SnapshotLagConfig>, epochs_to_keep: Option<u64>) -> Self {
        Self::Writer {
            snapshot_config: snapshot_config.unwrap_or_default(),
            pruning_options: PruningOptions { epochs_to_keep },
        }
    }
}

pub async fn start_test_indexer(
    db_url: String,
    rpc_url: String,
    reader_writer_config: ReaderWriterConfig,
    data_ingestion_path: PathBuf,
) -> (PgIndexerStore, JoinHandle<Result<(), IndexerError>>, CancellationToken) {
    let token = CancellationToken::new();
    let (store, handle) =
        start_test_indexer_impl(db_url, rpc_url, reader_writer_config, Some(data_ingestion_path), token.clone()).await;
    (store, handle, token)
}

/// Starts an indexer reader or writer for testing depending on the `reader_writer_config`.
pub async fn start_test_indexer_impl(
    db_url: String,
    rpc_url: String,
    reader_writer_config: ReaderWriterConfig,
    data_ingestion_path: Option<PathBuf>,
    cancel: CancellationToken,
) -> (PgIndexerStore, JoinHandle<Result<(), IndexerError>>) {
    // Reduce the connection pool size to 5 for testing
    // to prevent maxing out
    let pool_config = ConnectionPoolConfig {
        pool_size: 5,
        connection_timeout: Duration::from_secs(10),
        statement_timeout: Duration::from_secs(30),
    };

    println!("db_url: {db_url}");
    println!("pool_config: {pool_config:?}");
    println!("{data_ingestion_path:?}");

    let registry = prometheus::Registry::default();

    init_metrics(&registry);

    let indexer_metrics = IndexerMetrics::new(&registry);

    let pool = ConnectionPool::new(db_url.parse().unwrap(), pool_config).await.unwrap();
    let store = PgIndexerStore::new(pool.clone(), UploadOptions::default(), indexer_metrics.clone());

    let handle = match reader_writer_config {
        ReaderWriterConfig::Reader { reader_mode_rpc_url } => {
            let config = crate::config::JsonRpcConfig {
                name_service_options: crate::config::NameServiceOptions::default(),
                rpc_address: reader_mode_rpc_url.parse().unwrap(),
                rpc_client_url: rpc_url,
            };
            tokio::spawn(async move { Indexer::start_reader(&config, &registry, pool).await })
        }
        ReaderWriterConfig::Writer { snapshot_config, pruning_options } => {
            let connection = Connection::dedicated(&db_url.parse().unwrap()).await.unwrap();
            crate::db::reset_database(connection).await.unwrap();

            let store_clone = store.clone();
            let mut ingestion_config = IngestionConfig::default();
            ingestion_config.sources.data_ingestion_path = data_ingestion_path;

            tokio::spawn(async move {
                Indexer::start_writer_with_config(
                    &ingestion_config,
                    store_clone,
                    indexer_metrics,
                    snapshot_config,
                    pruning_options,
                    cancel,
                )
                .await
            })
        }
    };

    (store, handle)
}

#[derive(Clone)]
pub struct SuiTransactionBlockResponseBuilder<'a> {
    response: SuiTransactionBlockResponse,
    full_response: &'a SuiTransactionBlockResponse,
}

impl<'a> SuiTransactionBlockResponseBuilder<'a> {
    pub fn new(full_response: &'a SuiTransactionBlockResponse) -> Self {
        Self { response: SuiTransactionBlockResponse::default(), full_response }
    }

    pub fn with_input(mut self) -> Self {
        self.response =
            SuiTransactionBlockResponse { transaction: self.full_response.transaction.clone(), ..self.response };
        self
    }

    pub fn with_raw_input(mut self) -> Self {
        self.response =
            SuiTransactionBlockResponse { raw_transaction: self.full_response.raw_transaction.clone(), ..self.response };
        self
    }

    pub fn with_effects(mut self) -> Self {
        self.response = SuiTransactionBlockResponse { effects: self.full_response.effects.clone(), ..self.response };
        self
    }

    pub fn with_events(mut self) -> Self {
        self.response = SuiTransactionBlockResponse { events: self.full_response.events.clone(), ..self.response };
        self
    }

    pub fn with_balance_changes(mut self) -> Self {
        self.response =
            SuiTransactionBlockResponse { balance_changes: self.full_response.balance_changes.clone(), ..self.response };
        self
    }

    pub fn with_object_changes(mut self) -> Self {
        self.response =
            SuiTransactionBlockResponse { object_changes: self.full_response.object_changes.clone(), ..self.response };
        self
    }

    pub fn with_input_and_changes(mut self) -> Self {
        self.response = SuiTransactionBlockResponse {
            transaction: self.full_response.transaction.clone(),
            balance_changes: self.full_response.balance_changes.clone(),
            object_changes: self.full_response.object_changes.clone(),
            ..self.response
        };
        self
    }

    pub fn build(self) -> SuiTransactionBlockResponse {
        SuiTransactionBlockResponse {
            transaction: self.response.transaction,
            raw_transaction: self.response.raw_transaction,
            effects: self.response.effects,
            events: self.response.events,
            balance_changes: self.response.balance_changes,
            object_changes: self.response.object_changes,
            // Use full response for any fields that aren't showable
            ..self.full_response.clone()
        }
    }
}

/// Set up a test indexer fetching from a REST endpoint served by the given Simulacrum.
pub async fn set_up(
    sim: Arc<Simulacrum>,
    data_ingestion_path: PathBuf,
) -> (JoinHandle<()>, PgIndexerStore, JoinHandle<Result<(), IndexerError>>, TempDb) {
    let database = TempDb::new().unwrap();
    let server_url: SocketAddr = format!("127.0.0.1:{}", get_available_port()).parse().unwrap();

    let server_handle = tokio::spawn(async move {
        sui_rest_api::RestService::new_without_version(sim).start_service(server_url).await;
    });
    // Starts indexer
    let (pg_store, pg_handle, _) = start_test_indexer(
        database.database().url().as_str().to_owned(),
        format!("http://{}", server_url),
        ReaderWriterConfig::writer_mode(None, None),
        data_ingestion_path,
    )
    .await;
    (server_handle, pg_store, pg_handle, database)
}

/// Wait for the indexer to catch up to the given checkpoint sequence number.
pub async fn wait_for_checkpoint(
    pg_store: &PgIndexerStore,
    checkpoint_sequence_number: u64,
) -> Result<(), IndexerError> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while {
            let cp_opt = pg_store.get_latest_checkpoint_sequence_number().await.unwrap();
            cp_opt.is_none() || (cp_opt.unwrap() < checkpoint_sequence_number)
        } {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Timeout waiting for indexer to catchup to checkpoint");
    Ok(())
}
