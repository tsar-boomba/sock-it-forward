use std::{net::SocketAddr, sync::Arc};

use arc_swap::ArcSwapOption;
use color_eyre::eyre::Context;
use iroh::{Endpoint, EndpointAddr, endpoint::{RecvStream, SendStream, presets}};
use tokio::{io::AsyncWriteExt, net::TcpListener, select};

const ALPN: &[u8] = b"ibomb-sock-it-forward-0";

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    Ok(())
}

struct PublicConfig {
    addr: SocketAddr,
}

#[derive(Clone)]
struct PublicState {
    iroh_conn: Arc<ArcSwapOption<iroh::endpoint::Connection>>,
}

struct PublicServer {
    listener: Arc<TcpListener>,
    state: Arc<PublicState>,
}

impl PublicServer {
    async fn accept(&self) -> color_eyre::Result<()> {
        let (mut stream, peer) = self.listener.accept().await.context("failed to accept")?;
        println!("forwarding conn from {peer}");

        tokio::task::spawn_local({
            let state = self.state.clone();
            async move {
                let conn_opt = state.iroh_conn.load();
                let Some(iroh_conn) = conn_opt.as_deref() else {
                    stream.shutdown().await.ok();
                    return;
                };

                let (mut send, mut recv) = iroh_conn.open_bi().await.unwrap();

                // TODO: write proxy protocol stuff into tcp stream before starting copying

                let (mut tcp_recv, mut tcp_send) = stream.split();

                let (quic_to_tcp_res, tcp_to_quic_res) = tokio::join!(
                    tokio::io::copy(&mut recv, &mut tcp_send),
                    tokio::io::copy(&mut tcp_recv, &mut send)
                );

                send.finish().ok();
                recv.stop(0u32.into()).ok();
            }
        });

        Ok(())
    }

    async fn run(self) -> ! {
        loop {
            if let Err(err) = self.accept().await {
                println!("accept err: {err}");
            }
        }
    }
}

async fn public_side(config: PublicConfig) -> color_eyre::Result<()> {
    let listener = Arc::new(TcpListener::bind(config.addr).await?);
    let endpoint = Endpoint::bind(presets::N0).await?;
    endpoint.set_alpns(vec![ALPN.to_vec()]);
    endpoint.online().await;
    // Get the endpoint's address so that we can connect to it.
    let addr = endpoint.addr();
    println!("server at {addr:#?}");

    let state = Arc::new(PublicState {
        iroh_conn: Arc::new(ArcSwapOption::empty()),
    });

    // Spawn up to n-1 threads for handling incoming connections
    for _ in 0..(num_cpus::get() - 1).max(1) {
        let server  = PublicServer {
            listener: listener.clone(),
            state: state.clone(),
        };

        std::thread::spawn(move || {
            tokio::runtime::LocalRuntime::new()
                .unwrap()
                .block_on(server.run())
        });
    }

    while let Some(incoming) = endpoint.accept().await {
        match incoming.await {
            Ok(conn) => {
                state.iroh_conn.swap(Some(Arc::new(conn)));
            }
            Err(_) => todo!(),
        }
    }

    Ok(())
}


struct PrivateConfig {
    target_addr: SocketAddr,
    public_endpoint_addr: EndpointAddr,
}

struct PrivateServer {
    target_addr: SocketAddr,
    iroh_conn_recv: tokio::sync::broadcast::Receiver<iroh::endpoint::Connection>,
}

impl PrivateServer {
    async fn handle_stream(target_addr: SocketAddr, send: SendStream, recv: RecvStream) -> color_eyre::Result<()> {
        // TODO: open stream to target_addr and copy bytes
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
                    tokio::task::spawn_local(Self::handle_stream(self.target_addr, send, recv));
                }
            }
        }
    }
}

async fn private_side(config: PrivateConfig) -> color_eyre::Result<()> {
    let endpoint = Endpoint::bind(presets::N0).await?;
    endpoint.set_alpns(vec![ALPN.to_vec()]);
    endpoint.online().await;
    // Get the endpoint's address so that we can connect to it.
    let addr = endpoint.addr();
    println!("server at {addr:#?}");

    let (mut conn_send, conn_recv) = tokio::sync::broadcast::channel(1);

    // Spawn up to n-1 threads for handling incoming connections
    for _ in 0..(num_cpus::get() - 1).max(1) {
        let server  = PrivateServer {
            iroh_conn_recv: conn_recv.resubscribe(),
            target_addr: config.target_addr,
        };

        std::thread::spawn(move || {
            tokio::runtime::LocalRuntime::new()
                .unwrap()
                .block_on(server.run())
        });
    }

    while let Some(incoming) = endpoint.accept().await {
        match incoming.await {
            Ok(conn) => {
                conn_send.send(conn);
            }
            Err(_) => todo!(),
        }
    }

    Ok(())
}
