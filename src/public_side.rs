use std::{net::SocketAddr, sync::Arc};

use arc_swap::ArcSwapOption;
use iroh::{Endpoint, endpoint::presets};
use proxy_protocol::{
    ProxyHeader,
    version2::{ProxyAddresses, ProxyCommand, ProxyTransportProtocol},
};
use tokio::io::AsyncWriteExt;

use crate::{
    ALPN,
    listener::{Accepted, Listeners},
};

pub struct PublicConfig {
    pub addrs: Vec<SocketAddr>,
    /// The one and only key pair allowed to connect the private
    /// to the private side to the public side of the forwarder
    pub allowed_peer_key: Option<iroh::PublicKey>,
    pub secret_key: iroh::SecretKey,
}

#[derive(Clone)]
struct PublicState {
    iroh_conn: Arc<ArcSwapOption<iroh::endpoint::Connection>>,
}

struct PublicServer {
    listeners: Arc<Listeners>,
    state: Arc<PublicState>,
}

impl PublicServer {
    async fn accept(&self) -> color_eyre::Result<()> {
        let Accepted {
            mut stream,
            local_addr,
            remote_addr,
        } = self.listeners.accept().await;
        tracing::debug!("forwarding conn on {local_addr} from {remote_addr}");

        tokio::task::spawn_local({
            let state = self.state.clone();
            async move {
                let conn_opt = state.iroh_conn.load();
                let Some(iroh_conn) = conn_opt.as_deref() else {
                    tracing::warn!("got conn without private side connected!");
                    return;
                };

                let Ok((mut send, mut recv)) = iroh_conn.open_bi().await else {
                    tracing::warn!("Couldn't open new bidi");
                    return;
                };

                // Write connection information into stream before data
                // this is used on the private side for routing. Private side
                // can also choose to passthrough to downstream servers
                let proxy_header = create_proxy_header(local_addr, remote_addr);
                if let Err(err) = send
                    .write_all(&*proxy_protocol::encode(proxy_header).unwrap())
                    .await
                {
                    tracing::error!("Error wirting proxy header: {err:?}");
                    return;
                };

                let (mut tcp_recv, mut tcp_send) = stream.split();

                // Poll both until either fails
                tokio::select! {
                    _ = tokio::io::copy(&mut recv, &mut tcp_send) => {}
                    _ = tokio::io::copy(&mut tcp_recv, &mut send) => {}
                };

                send.reset(0u32.into()).ok();
                recv.stop(0u32.into()).ok();
            }
        });

        Ok(())
    }

    async fn run(self) -> ! {
        loop {
            if let Err(err) = self.accept().await {
                tracing::error!("accept err: {err}");
            }
        }
    }
}

pub async fn public_side(config: PublicConfig) -> color_eyre::Result<()> {
    let listeners = Arc::new(Listeners::new(&config.addrs).await?);

    for addr in listeners.addrs() {
        tracing::info!("TCP listening at: {}", addr);
    }

    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(config.secret_key)
        .bind()
        .await?;
    endpoint.set_alpns(vec![ALPN.to_vec()]);
    endpoint.online().await;
    // Get the endpoint's address so that we can connect to it.
    let addr = endpoint.addr();
    tracing::info!("server iroh at {addr:#?}");
    tracing::info!("server public key: {}", addr.id);

    let state = Arc::new(PublicState {
        iroh_conn: Arc::new(ArcSwapOption::empty()),
    });

    // Spawn up to n-1 threads for handling incoming connections
    for _ in 0..(num_cpus::get() - 1).max(1) {
        let server = PublicServer {
            listeners: listeners.clone(),
            state: state.clone(),
        };

        std::thread::spawn(move || {
            tokio::runtime::LocalRuntime::new()
                .unwrap()
                .block_on(server.run())
        });
    }

    tracing::info!("waiting for private side to connect...");
    while let Some(incoming) = endpoint.accept().await {
        match incoming.await {
            Ok(conn) => {
                if let Some(allowed_remote_id) = config.allowed_peer_key.as_ref() {
                    if conn.remote_id() != *allowed_remote_id {
                        tracing::info!("unrecognized private side tried to connect!");
                        conn.close(0u32.into(), b"");
                        continue;
                    }
                }

                tracing::info!("got new conn from private side!");
                state.iroh_conn.swap(Some(Arc::new(conn)));
            }
            Err(err) => {
                tracing::error!("Error accepting iroh conn: {err:?}")
            },
        }
    }

    Ok(())
}

fn create_proxy_header(local_addr: SocketAddr, peer_addr: SocketAddr) -> ProxyHeader {
    ProxyHeader::Version2 {
        command: ProxyCommand::Proxy,
        transport_protocol: ProxyTransportProtocol::Stream,
        addresses: match peer_addr {
            SocketAddr::V4(socket_addr_v4) => ProxyAddresses::Ipv4 {
                source: socket_addr_v4,
                destination: match local_addr {
                    SocketAddr::V4(socket_addr_v4) => socket_addr_v4,
                    SocketAddr::V6(_) => unreachable!(),
                },
            },
            SocketAddr::V6(socket_addr_v6) => ProxyAddresses::Ipv6 {
                source: socket_addr_v6,
                destination: match local_addr {
                    SocketAddr::V4(_) => unreachable!(),
                    SocketAddr::V6(socket_addr_v6) => socket_addr_v6,
                },
            },
        },
    }
}
