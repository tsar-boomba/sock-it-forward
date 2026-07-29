use std::{
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
};

use arc_swap::ArcSwapOption;
use color_eyre::eyre::Context;
use iroh::{Endpoint, endpoint::presets};
use proxy_protocol::{
    ProxyHeader,
    version2::{ProxyAddresses, ProxyCommand, ProxyTransportProtocol},
};
use tokio::{io::AsyncWriteExt, net::TcpListener};

use crate::ALPN;

pub struct PublicConfig {
    pub addr: SocketAddr,
    /// Write proxy info to iroh streams with the [Proxy Protocol](http://example.org)
    pub proxy_protocol: bool,
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
    listener: Arc<TcpListener>,
    state: Arc<PublicState>,
    proxy_protocol: bool,
}

impl PublicServer {
    async fn accept(&self) -> color_eyre::Result<()> {
        let (mut stream, peer_addr) = self.listener.accept().await.context("failed to accept")?;
        tracing::debug!("forwarding conn from {peer_addr}");

        tokio::task::spawn_local({
            let state = self.state.clone();
            let proxy_protocol = self.proxy_protocol;
            async move {
                let conn_opt = state.iroh_conn.load();
                let Some(iroh_conn) = conn_opt.as_deref() else {
                    stream.shutdown().await.ok();
                    return;
                };

                let (mut send, mut recv) = iroh_conn.open_bi().await.unwrap();

                if proxy_protocol {
                    let proxy_header = create_proxy_header(peer_addr);
                    if let Err(err) = send
                        .write_all(&*proxy_protocol::encode(proxy_header).unwrap())
                        .await
                    {
                        tracing::error!("Error wirting proxy header: {err:?}");
                        return;
                    };
                }

                let (mut tcp_recv, mut tcp_send) = stream.split();

                let (_quic_to_tcp_res, _tcp_to_quic_res) = tokio::join!(
                    tokio::io::copy(&mut recv, &mut tcp_send),
                    tokio::io::copy(&mut tcp_recv, &mut send)
                );

                send.reset(0u32.into()).ok();
                recv.stop(0u32.into()).ok();
                stream.shutdown().await.ok();
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
    let listener = Arc::new(TcpListener::bind(config.addr).await?);
    tracing::info!("TCP listening at: {}", config.addr);
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
            listener: listener.clone(),
            state: state.clone(),
            proxy_protocol: config.proxy_protocol,
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
            Err(_) => todo!(),
        }
    }

    Ok(())
}

fn create_proxy_header(peer_addr: SocketAddr) -> ProxyHeader {
    ProxyHeader::Version2 {
        command: ProxyCommand::Proxy,
        transport_protocol: ProxyTransportProtocol::Stream,
        addresses: match peer_addr {
            SocketAddr::V4(socket_addr_v4) => ProxyAddresses::Ipv4 {
                source: socket_addr_v4,
                destination: SocketAddrV4::new(Ipv4Addr::from_octets([0; 4]), 0),
            },
            SocketAddr::V6(socket_addr_v6) => ProxyAddresses::Ipv6 {
                source: socket_addr_v6,
                destination: SocketAddrV6::new(Ipv6Addr::from_octets([0; 16]), 0, 0, 0),
            },
        },
    }
}
