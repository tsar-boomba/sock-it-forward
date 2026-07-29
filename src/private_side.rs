use std::{net::SocketAddr, time::Duration};

use iroh::{Endpoint, EndpointAddr, endpoint::{RecvStream, SendStream, presets}};
use tokio::{io::AsyncWriteExt, net::TcpStream, select};

use crate::ALPN;


pub struct PrivateConfig {
    pub target_addr: SocketAddr,
    pub public_endpoint_addr: EndpointAddr,
    /// keypair that identifies this endpoint.    
    pub secret_key: iroh::SecretKey,
}

struct PrivateServer {
    target_addr: SocketAddr,
    iroh_conn_recv: tokio::sync::broadcast::Receiver<iroh::endpoint::Connection>,
}

impl PrivateServer {
    async fn handle_stream(
        target_addr: SocketAddr,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> color_eyre::Result<()> {
        let mut stream = TcpStream::connect(target_addr).await?;
        let (mut tcp_recv, mut tcp_send) = stream.split();

        let (_quic_to_tcp_res, _tcp_to_quic_res) = tokio::join!(
            tokio::io::copy(&mut recv, &mut tcp_send),
            tokio::io::copy(&mut tcp_recv, &mut send)
        );

        send.finish().ok();
        recv.stop(0u32.into()).ok();
        stream.shutdown().await.ok();

        Ok(())
    }

    async fn run(mut self) -> ! {
        let mut conn = self.iroh_conn_recv.recv().await.unwrap();
        loop {
            select! {
                new_conn = self.iroh_conn_recv.recv() => {
                    if let Ok(new_conn) = new_conn {
                        conn = new_conn;
                    }
                },
                stream = conn.accept_bi() => {
                    let Ok((send, recv)) = stream else {
                        continue;
                    };
                    tracing::debug!("Got stream from public side! Forawding to {}...", self.target_addr);
                    tokio::task::spawn_local(Self::handle_stream(self.target_addr, send, recv));
                }
            }
        }
    }
}

pub async fn private_side(config: PrivateConfig) -> color_eyre::Result<()> {
    let endpoint = Endpoint::builder(presets::N0).secret_key(config.secret_key).bind().await?;
    endpoint.set_alpns(vec![ALPN.to_vec()]);
    endpoint.online().await;
    // Get the endpoint's address so that we can connect to it.
    let addr = endpoint.addr();
    tracing::info!("server at {addr:#?}");

    let (conn_send, conn_recv) = tokio::sync::broadcast::channel(1);

    // Spawn up to n-1 threads for handling incoming connections
    for _ in 0..(num_cpus::get() - 1).max(1) {
        let server = PrivateServer {
            iroh_conn_recv: conn_recv.resubscribe(),
            target_addr: config.target_addr,
        };

        std::thread::spawn(move || {
            tokio::runtime::LocalRuntime::new()
                .unwrap()
                .block_on(server.run())
        });
    }

    loop {
        tracing::info!(
            "connecting to public side at {:?}...",
            config.public_endpoint_addr
        );
        let Ok(conn) = endpoint
            .connect(config.public_endpoint_addr.clone(), ALPN)
            .await
        else {
            tracing::error!("failed to connected to public side, retrying soon...");
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        conn.set_max_concurrent_bi_streams((u16::MAX >> 2).into());

        tracing::info!("connected to public side!");
        conn_send.send(conn.clone()).unwrap();
        conn.closed().await;
        tracing::info!("public side connection closed!");
    }
}
