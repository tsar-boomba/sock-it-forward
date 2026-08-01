mod listener;
mod port_mapping;
mod private_side;
mod public_side;

use std::{
    net::SocketAddr, path::{Path, PathBuf}, sync::Arc,
};

use clap::Parser;
use color_eyre::eyre::Context;
use tracing_subscriber::EnvFilter;

use crate::{
    port_mapping::PortMapping, private_side::{PrivateConfig, private_side}, public_side::{PublicConfig, public_side},
};

const ALPN: &[u8] = b"/sock-it-forward/0";

#[derive(clap::Parser)]
enum Command {
    Public {
        #[arg(short, long)]
        addrs: Vec<SocketAddr>,
        /// Only allowed private-side public key in Base64 or hex
        #[arg(short = 'k', long)]
        private_side_key: Option<iroh::PublicKey>,
        /// Loads/creates a key in this file
        #[arg(short, long)]
        secret_key: Option<PathBuf>,
    },
    Private {
        #[arg(short, long)]
        public_side_key: iroh::PublicKey,
        /// Loads/creates a key in this file
        #[arg(short, long)]
        secret_key: Option<PathBuf>,
        #[arg(short = 'm', long = "map", required = true)]
        mappings: Vec<PortMapping>,
    },
}

fn main() -> color_eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or(EnvFilter::new("info,iroh::net_report::report=error,sock_it_forward=debug")),
        )
        .init();

    let command = Command::parse();

    let rt = tokio::runtime::LocalRuntime::new().unwrap();
    match command {
        Command::Public {
            addrs,
            private_side_key,
            secret_key,
        } => {
            rt.block_on(public_side(PublicConfig {
                addrs,
                allowed_peer_key: private_side_key,
                secret_key: if let Some(path) = secret_key {
                    load_secret_key(&path)?
                } else {
                    iroh::SecretKey::generate()
                },
            }))?;
        }
        Command::Private {
            public_side_key,
            secret_key,
            mappings,
        } => {
            rt.block_on(private_side(PrivateConfig {
                public_endpoint_addr: iroh::EndpointAddr::new(public_side_key),
                port_mappings: Arc::from(mappings),
                secret_key: if let Some(path) = secret_key {
                    // load/create file
                    load_secret_key(&path)?
                } else {
                    // new key
                    iroh::SecretKey::generate()
                },
            }))?;
        }
    }

    Ok(())
}

/// Loads key from `path` or generate a key and stores it at `path`
fn load_secret_key(path: &Path) -> color_eyre::Result<iroh::SecretKey> {
    let key = if std::fs::exists(path)? {
        let bytes = std::fs::read(path).context("reading and validating file")?;
        iroh::SecretKey::from_bytes(bytes.as_slice().try_into()?)
    } else {
        let key = iroh::SecretKey::generate();
        std::fs::write(path, key.to_bytes())?;
        key
    };

    // Also write public key for reference
    std::fs::write(path.with_added_extension("pub"), key.public().as_bytes())?;

    Ok(key)
}
