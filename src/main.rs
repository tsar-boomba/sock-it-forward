mod private_side;
mod public_side;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use clap::Parser;
use color_eyre::eyre::Context;
use tracing_subscriber::EnvFilter;

use crate::{
    private_side::{PrivateConfig, private_side},
    public_side::{PublicConfig, public_side},
};

const ALPN: &[u8] = b"/sock-it-forward/0";

#[derive(clap::Parser)]
enum Command {
    Public {
        #[arg(short, long)]
        addr: SocketAddr,
        #[arg(short, long, default_value = "true")]
        proxy_protocol: bool,
        /// Only allowed private-side public key in Base64 or hex
        #[arg(short = 'k', long)]
        private_side_key: Option<iroh::PublicKey>,
        /// Loads/creates a key in this file
        #[arg(short, long)]
        secret_key: Option<PathBuf>,
    },
    Private {
        #[arg(short = 'a', long)]
        target_addr: SocketAddr,
        #[arg(short, long)]
        public_side_key: iroh::PublicKey,
        /// Loads/creates a key in this file
        #[arg(short, long)]
        secret_key: Option<PathBuf>,
    },
}

fn main() -> color_eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or(EnvFilter::new("info,sock_it_forward=debug")),
        )
        .init();

    let command = Command::parse();

    let rt = tokio::runtime::LocalRuntime::new().unwrap();
    match command {
        Command::Public {
            addr,
            proxy_protocol,
            private_side_key,
            secret_key,
        } => {
            rt.block_on(public_side(PublicConfig {
                addr,
                proxy_protocol,
                allowed_peer_key: private_side_key,
                secret_key: if let Some(path) = secret_key {
                    load_secret_key(&path)?
                } else {
                    iroh::SecretKey::generate()
                },
            }))?;
        }
        Command::Private {
            target_addr,
            public_side_key,
            secret_key,
        } => {
            rt.block_on(private_side(PrivateConfig {
                target_addr,
                public_endpoint_addr: iroh::EndpointAddr::new(public_side_key),
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
