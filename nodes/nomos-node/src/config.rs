// std
use std::{
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
};
// crates
use cl::{InputWitness, NoteWitness, NullifierSecret};
use clap::{Parser, ValueEnum};
use color_eyre::eyre::{eyre, Result};
use hex::FromHex;
use nomos_core::staking::NMO_UNIT;
use nomos_da_network_service::backends::libp2p::validator::DaNetworkValidatorBackend;
use nomos_da_network_service::NetworkService as DaNetworkService;
use nomos_libp2p::{ed25519::SecretKey, Multiaddr};
use nomos_log::{Logger, LoggerBackend, LoggerFormat};
use nomos_network::backends::libp2p::Libp2p as NetworkBackend;
use nomos_network::NetworkService;
use nomos_storage::backends::rocksdb::RocksBackend;
use overwatch_rs::services::ServiceData;
use serde::{Deserialize, Serialize};
use subnetworks_assignations::versions::v1::FillFromNodeList;
use tracing::Level;
// internal
use crate::{NomosApiService, Wire};

#[derive(ValueEnum, Clone, Debug, Default)]
pub enum LoggerBackendType {
    Gelf,
    File,
    #[default]
    Stdout,
    Stderr,
}

#[derive(Parser, Debug, Clone)]
pub struct LogArgs {
    /// Address for the Gelf backend
    #[clap(long = "log-addr", env = "LOG_ADDR", required_if_eq("backend", "Gelf"))]
    log_addr: Option<String>,

    /// Directory for the File backend
    #[clap(long = "log-dir", env = "LOG_DIR", required_if_eq("backend", "File"))]
    directory: Option<PathBuf>,

    /// Prefix for the File backend
    #[clap(long = "log-path", env = "LOG_PATH", required_if_eq("backend", "File"))]
    prefix: Option<PathBuf>,

    /// Backend type
    #[clap(long = "log-backend", env = "LOG_BACKEND", value_enum)]
    backend: Option<LoggerBackendType>,

    #[clap(long = "log-format", env = "LOG_FORMAT")]
    format: Option<String>,

    #[clap(long = "log-level", env = "LOG_LEVEL")]
    level: Option<String>,
}

#[derive(Parser, Debug, Clone)]
pub struct NetworkArgs {
    #[clap(long = "net-host", env = "NET_HOST")]
    host: Option<IpAddr>,

    #[clap(long = "net-port", env = "NET_PORT")]
    port: Option<usize>,

    #[clap(long = "net-node-key", env = "NET_NODE_KEY")]
    node_key: Option<String>,

    #[clap(long = "net-initial-peers", env = "NET_INITIAL_PEERS", num_args = 1.., value_delimiter = ',')]
    pub initial_peers: Option<Vec<Multiaddr>>,
}

#[derive(Parser, Debug, Clone)]
pub struct HttpArgs {
    #[clap(long = "http-host", env = "HTTP_HOST")]
    http_addr: Option<SocketAddr>,

    #[clap(long = "http-cors-origin", env = "HTTP_CORS_ORIGIN")]
    pub cors_origins: Option<Vec<String>>,
}

#[derive(Parser, Debug, Clone)]
pub struct CryptarchiaArgs {
    #[clap(long = "consensus-chain-start", env = "CONSENSUS_CHAIN_START")]
    chain_start_time: Option<i64>,

    #[clap(long = "consensus-slot-duration", env = "CONSENSUS_SLOT_DURATION")]
    slot_duration: Option<u64>,

    #[clap(
        long = "consensus-note-sk",
        env = "CONSENSUS_NOTE_SK",
        requires("note_value")
    )]
    note_secret_key: Option<String>,

    #[clap(
        long = "consensus-note-value",
        env = "CONSENSUS_NOTE_VALUE",
        requires("note_secret_key")
    )]
    note_value: Option<u32>,
}

#[derive(Parser, Debug, Clone)]
pub struct MetricsArgs {
    #[clap(long = "with-metrics", env = "WITH_METRICS")]
    pub with_metrics: bool,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Config {
    pub log: <Logger as ServiceData>::Settings,
    pub network: <NetworkService<NetworkBackend> as ServiceData>::Settings,
    pub da_network:
        <DaNetworkService<DaNetworkValidatorBackend<FillFromNodeList>> as ServiceData>::Settings,
    pub da_indexer: <crate::DaIndexer as ServiceData>::Settings,
    pub da_verifier: <crate::DaVerifier as ServiceData>::Settings,
    pub da_sampling: <crate::DaSampling as ServiceData>::Settings,
    pub http: <NomosApiService as ServiceData>::Settings,
    pub cryptarchia: <crate::Cryptarchia as ServiceData>::Settings,
    pub storage: <crate::StorageService<RocksBackend<Wire>> as ServiceData>::Settings,
    pub wait_online_secs: u64,
}

impl Config {
    pub fn update_log(mut self, log_args: LogArgs) -> Result<Self> {
        let LogArgs {
            backend,
            log_addr: addr,
            directory,
            prefix,
            format,
            level,
        } = log_args;

        // Override the file config with the one from env variables.
        if let Some(backend) = backend {
            self.log.backend = match backend {
                LoggerBackendType::Gelf => LoggerBackend::Gelf {
                    addr: addr
                        .ok_or_else(|| eyre!("Gelf backend requires an address."))?
                        .to_socket_addrs()?
                        .next()
                        .ok_or_else(|| eyre!("Invalid gelf address"))?,
                },
                LoggerBackendType::File => LoggerBackend::File {
                    directory: directory
                        .ok_or_else(|| eyre!("File backend requires a directory."))?,
                    prefix,
                },
                LoggerBackendType::Stdout => LoggerBackend::Stdout,
                LoggerBackendType::Stderr => LoggerBackend::Stderr,
            }
        };

        // Update parts of the config.
        if let Some(format_str) = format {
            self.log.format = match format_str.as_str() {
                "Json" => LoggerFormat::Json,
                "Plain" => LoggerFormat::Plain,
                _ => return Err(eyre!("Invalid log format provided.")),
            };
        }
        if let Some(level_str) = level {
            self.log.level = match level_str.as_str() {
                "DEBUG" => Level::DEBUG,
                _ => return Err(eyre!("Invalid log level provided.")),
            };
        }
        Ok(self)
    }

    pub fn update_network(mut self, network_args: NetworkArgs) -> Result<Self> {
        let NetworkArgs {
            host,
            port,
            node_key,
            initial_peers,
        } = network_args;

        if let Some(IpAddr::V4(h)) = host {
            self.network.backend.inner.host = h;
        } else if host.is_some() {
            return Err(eyre!("Unsupported ip version"));
        }

        if let Some(port) = port {
            self.network.backend.inner.port = port as u16;
        }

        if let Some(node_key) = node_key {
            let mut key_bytes = hex::decode(node_key)?;
            self.network.backend.inner.node_key =
                SecretKey::try_from_bytes(key_bytes.as_mut_slice())?;
        }

        if let Some(peers) = initial_peers {
            self.network.backend.initial_peers = peers;
        }

        // TODO: configure mixclient and mixnode if the mixnet feature is enabled

        Ok(self)
    }

    pub fn update_http(mut self, http_args: HttpArgs) -> Result<Self> {
        let HttpArgs {
            http_addr,
            cors_origins,
        } = http_args;

        if let Some(addr) = http_addr {
            self.http.backend_settings.address = addr;
        }

        if let Some(cors) = cors_origins {
            self.http.backend_settings.cors_origins = cors;
        }

        Ok(self)
    }

    pub fn update_cryptarchia_consensus(mut self, consensus_args: CryptarchiaArgs) -> Result<Self> {
        let CryptarchiaArgs {
            chain_start_time,
            slot_duration,
            note_secret_key,
            note_value,
        } = consensus_args;

        if let Some(start_time) = chain_start_time {
            self.cryptarchia.time.chain_start_time =
                time::OffsetDateTime::from_unix_timestamp(start_time)?;
        }

        if let Some(duration) = slot_duration {
            self.cryptarchia.time.slot_duration = std::time::Duration::from_secs(duration);
        }

        if let Some(sk) = note_secret_key {
            let sk = <[u8; 16]>::from_hex(sk)?;

            let value = note_value.expect("Should be available if coin sk provided");

            self.cryptarchia.notes.push(InputWitness::new(
                NoteWitness::basic(value as u64, NMO_UNIT, &mut rand::thread_rng()),
                NullifierSecret::from_bytes(sk),
            ));
        }

        Ok(self)
    }
}
