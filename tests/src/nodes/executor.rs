use std::ops::Range;
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{net::SocketAddr, process::Child};

use crate::adjust_timeout;
use crate::topology::configs::GeneralConfig;
use cryptarchia_consensus::CryptarchiaSettings;
use nomos_da_dispersal::backend::kzgrs::{DispersalKZGRSBackendSettings, EncoderSettings};
use nomos_da_dispersal::DispersalServiceSettings;
use nomos_da_indexer::storage::adapters::rocksdb::RocksAdapterSettings as IndexerStorageAdapterSettings;
use nomos_da_indexer::IndexerSettings;
use nomos_da_network_service::backends::libp2p::common::DaNetworkBackendSettings;
use nomos_da_network_service::{
    backends::libp2p::executor::DaNetworkExecutorBackendSettings, NetworkConfig as DaNetworkConfig,
};
use nomos_da_sampling::backend::kzgrs::KzgrsSamplingBackendSettings;
use nomos_da_sampling::storage::adapters::rocksdb::RocksAdapterSettings as SamplingStorageAdapterSettings;
use nomos_da_sampling::DaSamplingServiceSettings;
use nomos_da_verifier::backend::kzgrs::KzgrsDaVerifierSettings;
use nomos_da_verifier::storage::adapters::rocksdb::RocksAdapterSettings as VerifierStorageAdapterSettings;
use nomos_da_verifier::DaVerifierServiceSettings;
use nomos_executor::api::backend::AxumBackendSettings;
use nomos_executor::config::Config;
use nomos_mix::message_blend::{
    CryptographicProcessorSettings, MessageBlendSettings, TemporalSchedulerSettings,
};
use nomos_network::{backends::libp2p::Libp2pConfig, NetworkConfig};
use nomos_node::api::paths::{CL_METRICS, DA_GET_RANGE};
use nomos_node::RocksBackendSettings;
use tempfile::NamedTempFile;

use super::{create_tempdir, persist_tempdir, GetRangeReq, CLIENT};

const BIN_PATH: &str = "../target/debug/nomos-executor";

pub struct Executor {
    addr: SocketAddr,
    tempdir: tempfile::TempDir,
    child: Child,
    config: Config,
}

impl Drop for Executor {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Err(e) = persist_tempdir(&mut self.tempdir, "nomos-executor") {
                println!("failed to persist tempdir: {e}");
            }
        }

        if let Err(e) = self.child.kill() {
            println!("failed to kill the child process: {e}");
        }
    }
}

impl Executor {
    pub async fn spawn(mut config: Config) -> Self {
        let dir = create_tempdir().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        let config_path = file.path().to_owned();

        #[cfg(not(feature = "debug"))]
        {
            use crate::nodes::LOGS_PREFIX;
            use nomos_tracing::logging::local::FileConfig;
            use nomos_tracing_service::LoggerLayer;

            // setup logging so that we can intercept it later in testing
            config.tracing.logger = LoggerLayer::File(FileConfig {
                directory: dir.path().to_owned(),
                prefix: Some(LOGS_PREFIX.into()),
            });
        }

        config.storage.db_path = dir.path().join("db");
        config
            .da_sampling
            .storage_adapter_settings
            .blob_storage_directory = dir.path().to_owned();
        config
            .da_verifier
            .storage_adapter_settings
            .blob_storage_directory = dir.path().to_owned();
        config.da_indexer.storage.blob_storage_directory = dir.path().to_owned();

        serde_yaml::to_writer(&mut file, &config).unwrap();
        let child = Command::new(std::env::current_dir().unwrap().join(BIN_PATH))
            .arg(&config_path)
            .current_dir(dir.path())
            .stdout(Stdio::inherit())
            .spawn()
            .unwrap();
        let node = Self {
            addr: config.http.backend_settings.address,
            child,
            tempdir: dir,
            config,
        };
        tokio::time::timeout(adjust_timeout(Duration::from_secs(10)), async {
            node.wait_online().await
        })
        .await
        .unwrap();

        node
    }

    pub async fn get_indexer_range(
        &self,
        app_id: [u8; 32],
        range: Range<[u8; 8]>,
    ) -> Vec<([u8; 8], Vec<Vec<u8>>)> {
        CLIENT
            .post(format!("http://{}{}", self.addr, DA_GET_RANGE))
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&GetRangeReq { app_id, range }).unwrap())
            .send()
            .await
            .unwrap()
            .json::<Vec<([u8; 8], Vec<Vec<u8>>)>>()
            .await
            .unwrap()
    }

    async fn wait_online(&self) {
        loop {
            let res = self.get(CL_METRICS).await;
            if res.is_ok() && res.unwrap().status().is_success() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn get(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        CLIENT
            .get(format!("http://{}{}", self.addr, path))
            .send()
            .await
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
}

pub fn create_executor_config(config: GeneralConfig) -> Config {
    Config {
        network: NetworkConfig {
            backend: Libp2pConfig {
                inner: config.network_config.swarm_config,
                initial_peers: config.network_config.initial_peers,
            },
        },
        mix: nomos_mix_service::MixConfig {
            backend: config.mix_config.backend,
            persistent_transmission: Default::default(),
            message_blend: MessageBlendSettings {
                cryptographic_processor: CryptographicProcessorSettings {
                    private_key: config.mix_config.private_key.to_bytes(),
                    num_mix_layers: 1,
                },
                temporal_processor: TemporalSchedulerSettings {
                    max_delay_seconds: 2,
                },
            },
            membership: config.mix_config.membership,
        },
        cryptarchia: CryptarchiaSettings {
            leader_config: config.consensus_config.leader_config,
            config: config.consensus_config.ledger_config,
            genesis_state: config.consensus_config.genesis_state,
            time: config.consensus_config.time,
            transaction_selector_settings: (),
            blob_selector_settings: (),
            network_adapter_settings:
                cryptarchia_consensus::network::adapters::libp2p::LibP2pAdapterSettings {
                    topic: String::from(nomos_node::CONSENSUS_TOPIC),
                },
            mix_adapter_settings:
                cryptarchia_consensus::mix::adapters::libp2p::LibP2pAdapterSettings {
                    broadcast_settings:
                        nomos_mix_service::network::libp2p::Libp2pBroadcastSettings {
                            topic: String::from(nomos_node::CONSENSUS_TOPIC),
                        },
                },
        },
        da_network: DaNetworkConfig {
            backend: DaNetworkExecutorBackendSettings {
                validator_settings: DaNetworkBackendSettings {
                    node_key: config.da_config.node_key,
                    membership: config.da_config.membership,
                    addresses: config.da_config.addresses,
                    listening_address: config.da_config.listening_address,
                },
                num_subnets: config.da_config.num_subnets,
            },
        },
        da_indexer: IndexerSettings {
            storage: IndexerStorageAdapterSettings {
                blob_storage_directory: "./".into(),
            },
        },
        da_verifier: DaVerifierServiceSettings {
            verifier_settings: KzgrsDaVerifierSettings {
                sk: config.da_config.verifier_sk,
                index: config.da_config.verifier_index,
                global_params_path: config.da_config.global_params_path.clone(),
            },
            network_adapter_settings: (),
            storage_adapter_settings: VerifierStorageAdapterSettings {
                blob_storage_directory: "./".into(),
            },
        },
        tracing: config.tracing_config.tracing_settings,
        http: nomos_api::ApiServiceSettings {
            backend_settings: AxumBackendSettings {
                address: config.api_config.address,
                cors_origins: vec![],
            },
        },
        da_sampling: DaSamplingServiceSettings {
            sampling_settings: KzgrsSamplingBackendSettings {
                num_samples: config.da_config.num_samples,
                num_subnets: config.da_config.num_subnets,
                old_blobs_check_interval: config.da_config.old_blobs_check_interval,
                blobs_validity_duration: config.da_config.blobs_validity_duration,
            },
            storage_adapter_settings: SamplingStorageAdapterSettings {
                blob_storage_directory: "./".into(),
            },
            network_adapter_settings: (),
        },
        storage: RocksBackendSettings {
            db_path: "./db".into(),
            read_only: false,
            column_family: Some("blocks".into()),
        },
        da_dispersal: DispersalServiceSettings {
            backend: DispersalKZGRSBackendSettings {
                encoder_settings: EncoderSettings {
                    num_columns: config.da_config.num_subnets as usize,
                    with_cache: false,
                    global_params_path: config.da_config.global_params_path,
                },
                dispersal_timeout: Duration::from_secs(u64::MAX),
            },
        },
    }
}