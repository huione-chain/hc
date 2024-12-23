// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;
use sui_indexer::{
    backfill::backfill_runner::BackfillRunner,
    config::{Command, UploadOptions},
    database::ConnectionPool,
    db::{check_db_migration_consistency, reset_database, run_migrations},
    indexer::Indexer,
    metrics::{spawn_connection_pool_metric_collector, start_prometheus_server, IndexerMetrics},
    restorer::formal_snapshot::IndexerFormalSnapshotRestorer,
    store::PgIndexerStore,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = sui_indexer::config::IndexerConfig::parse();

    // NOTE: this is to print out tracing like info, warn & error.
    let _guard = telemetry_subscribers::TelemetryConfig::new().with_env().init();
    warn!(
        "WARNING: Sui indexer is still experimental and we expect occasional breaking changes that require backfills."
    );

    let (_registry_service, registry) = start_prometheus_server(opts.metrics_address)?;
    mysten_metrics::init_metrics(&registry);
    let indexer_metrics = IndexerMetrics::new(&registry);

    let pool = ConnectionPool::new(opts.database_url.clone(), opts.connection_pool_config.clone()).await?;
    spawn_connection_pool_metric_collector(indexer_metrics.clone(), pool.clone());

    match opts.command {
        Command::Indexer { ingestion_config, snapshot_config, pruning_options, upload_options } => {
            // Make sure to run all migrations on startup, and also serve as a compatibility check.
            run_migrations(pool.dedicated_connection().await?).await?;
            let store = PgIndexerStore::new(pool, upload_options, indexer_metrics.clone());

            Indexer::start_writer_with_config(
                &ingestion_config,
                store,
                indexer_metrics,
                snapshot_config,
                pruning_options,
                CancellationToken::new(),
            )
            .await?;
        }
        Command::JsonRpcService(json_rpc_config) => {
            check_db_migration_consistency(&mut pool.get().await?).await?;

            Indexer::start_reader(&json_rpc_config, &registry, pool).await?;
        }
        Command::ResetDatabase { force } => {
            if !force {
                return Err(anyhow::anyhow!("Resetting the DB requires use of the `--force` flag",));
            }

            reset_database(pool.dedicated_connection().await?).await?;
        }
        Command::RunMigrations => {
            run_migrations(pool.dedicated_connection().await?).await?;
        }
        Command::RunBackFill { start, end, runner_kind, backfill_config } => {
            let total_range = start..=end;
            BackfillRunner::run(runner_kind, pool, backfill_config, total_range).await;
        }
        Command::Restore(restore_config) => {
            let store = PgIndexerStore::new(pool, UploadOptions::default(), indexer_metrics.clone());
            let mut formal_restorer = IndexerFormalSnapshotRestorer::new(store, restore_config).await?;
            formal_restorer.restore().await?;
        }
    }

    Ok(())
}
