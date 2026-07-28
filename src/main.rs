mod private_side;
mod public_side;

use std::{net::SocketAddr, str::FromStr};

use clap::Parser;
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
    },
    Private {
        #[arg(short, long)]
        target_addr: SocketAddr,
        #[arg(short, long)]
        public_side_key: iroh::PublicKey,
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
        } => {
            rt.block_on(public_side(PublicConfig {
                addr,
                proxy_protocol,
                allowed_peer_key: private_side_key,
                secret_key: iroh::SecretKey::generate(),
            }))?;
        }
        Command::Private {
            target_addr,
            public_side_key,
        } => {
            rt.block_on(private_side(PrivateConfig {
                target_addr,
                public_endpoint_addr: iroh::EndpointAddr::new(public_side_key),
                secret_key: iroh::SecretKey::generate(),
            }))?;
        }
    }

    Ok(())
}
