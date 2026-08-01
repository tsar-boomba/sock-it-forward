use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
    time::Duration,
};

use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{RecvStream, SendStream, presets},
};
use tokio::{io::AsyncWriteExt, net::TcpStream, select};

use crate::{ALPN, port_mapping::PortMapping};

pub struct PrivateConfig {
    pub public_endpoint_addr: EndpointAddr,
    /// keypair that identifies this endpoint.    
    pub secret_key: iroh::SecretKey,
    pub port_mappings: Arc<[PortMapping]>,
}

struct PrivateServer {
    port_mappings: Arc<[PortMapping]>,
    iroh_conn_recv: tokio::sync::broadcast::Receiver<iroh::endpoint::Connection>,
}

impl PrivateServer {
    async fn handle_stream(
        port_mappings: Arc<[PortMapping]>,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> color_eyre::Result<()> {
        let proxy_info = read_proxy_header(&mut recv).await?;
        let Some(mapping) = port_mappings
            .iter()
            .find(|mapping| mapping.source_port == proxy_info.dest_addr.port())
        else {
            tracing::warn!("No mapping for {}", proxy_info.dest_addr.port());
            return Ok(());
        };

        tracing::debug!(
            "proxying from {} to {}",
            proxy_info.dest_addr,
            mapping.dest_addr
        );
        let mut stream = match TcpStream::connect(mapping.dest_addr).await {
            Ok(stream) => stream,
            Err(err) => {
                send.finish().ok();
                recv.stop(0u32.into()).ok();
                tracing::warn!("Failed to connect to upstream at {}", mapping.dest_addr);
                return Err(err.into());
            }
        };
        let (mut tcp_recv, mut tcp_send) = stream.split();

        if mapping.proxy_protocol {
            tcp_send.write_all(&proxy_info.raw_header).await?;
        }

        // Poll both until either fails
        tokio::select! {
            _ = tokio::io::copy(&mut recv, &mut tcp_send) => {}
            _ = tokio::io::copy(&mut tcp_recv, &mut send) => {}
        };

        send.finish().ok();
        recv.stop(0u32.into()).ok();

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
                    tokio::task::spawn_local(Self::handle_stream(self.port_mappings.clone(), send, recv));
                }
            }
        }
    }
}

pub async fn private_side(config: PrivateConfig) -> color_eyre::Result<()> {
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(config.secret_key)
        .bind()
        .await?;
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
            port_mappings: config.port_mappings.clone(),
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

#[derive(Debug)]
pub struct ProxyInfo {
    #[allow(dead_code)]
    pub source_addr: SocketAddr,
    /// SocketAddr on public side that accepted the connection. Use the port
    /// number here to determine the upstream to send to
    pub dest_addr: SocketAddr,
    /// The raw consumed header, in case it needs to be forwarded upstream.
    pub raw_header: Vec<u8>,
}

/// The 12-byte block that every PROXY protocol v2 header starts with.
const SIGNATURE: [u8; 12] = [
    0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A,
];

fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Reads (consumes) a PROXY protocol v2 header from the start of the stream.
///
/// After this returns Ok, the stream is positioned at the first byte of
/// application data. For a LOCAL command, falls back to the socket's real
/// peer/local addresses per the spec.
pub async fn read_proxy_header(stream: &mut RecvStream) -> io::Result<ProxyInfo> {
    // Fixed 16-byte prefix: 12 signature + 1 ver/cmd + 1 fam/proto + 2 length.
    let mut header = [0u8; 16];
    stream
        .read_exact(&mut header)
        .await
        .map_err(io::Error::other)?;

    if header[..12] != SIGNATURE {
        return Err(invalid("invalid PROXY protocol v2 signature"));
    }
    if header[12] >> 4 != 2 {
        return Err(invalid("unsupported PROXY protocol version"));
    }

    let command = header[12] & 0x0F; // 0 = LOCAL, 1 = PROXY, 2..=15 invalid
    let family = header[13] >> 4; // 0 = UNSPEC, 1 = AF_INET, 2 = AF_INET6, 3 = AF_UNIX
    let addr_len = u16::from_be_bytes([header[14], header[15]]) as usize;

    // Consume the whole address block (including any TLVs) regardless of
    // command, so the stream is left at the start of application data.
    let mut addrs = vec![0u8; addr_len];
    stream
        .read_exact(&mut addrs)
        .await
        .map_err(io::Error::other)?;

    let bytes_read = 16 + addr_len;
    let raw_header = {
        let mut raw = Vec::with_capacity(bytes_read);
        raw.extend_from_slice(&header);
        raw.extend_from_slice(&addrs);
        raw
    };

    match command {
        // LOCAL: no client to declare (health checks etc.). Spec says accept
        // the connection and use the real socket addresses.
        0 => Err(invalid(
            "public side sent local proxy info (should be impossible)",
        )),
        1 => match family {
            // AF_INET: src_addr[4] dst_addr[4] src_port[2] dst_port[2]
            1 => {
                if addrs.len() < 12 {
                    return Err(invalid("truncated IPv4 address block"));
                }
                let source_ip = Ipv4Addr::new(addrs[0], addrs[1], addrs[2], addrs[3]);
                let dest_ip = Ipv4Addr::new(addrs[4], addrs[5], addrs[6], addrs[7]);
                let source_port = u16::from_be_bytes([addrs[8], addrs[9]]);
                let dest_port = u16::from_be_bytes([addrs[10], addrs[11]]);

                Ok(ProxyInfo {
                    source_addr: SocketAddr::V4(SocketAddrV4::new(source_ip, source_port)),
                    dest_addr: SocketAddr::V4(SocketAddrV4::new(dest_ip, dest_port)),
                    raw_header,
                })
            }
            // AF_INET6: src_addr[16] dst_addr[16] src_port[2] dst_port[2]
            2 => {
                if addrs.len() < 36 {
                    return Err(invalid("truncated IPv6 address block"));
                }
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&addrs[0..16]);
                let source_ip = Ipv6Addr::from(octets);
                octets.copy_from_slice(&addrs[16..32]);
                let dest_ip = Ipv6Addr::from(octets);
                let source_port = u16::from_be_bytes([addrs[32], addrs[33]]);
                let dest_port = u16::from_be_bytes([addrs[34], addrs[35]]);

                Ok(ProxyInfo {
                    source_addr: SocketAddr::V6(SocketAddrV6::new(source_ip, source_port, 0, 0)),
                    dest_addr: SocketAddr::V6(SocketAddrV6::new(dest_ip, dest_port, 0, 0)),
                    raw_header,
                })
            }
            // AF_UNSPEC (0) or AF_UNIX (3) don't map to an IP SocketAddr.
            _ => Err(invalid("unsupported PROXY protocol address family")),
        },
        _ => Err(invalid("invalid PROXY protocol command")),
    }
}
