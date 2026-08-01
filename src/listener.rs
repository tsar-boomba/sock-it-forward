use std::{io, net::SocketAddr};

use futures_util::{StreamExt, stream::FuturesUnordered};

pub struct TcpListener {
    addr: SocketAddr,
    listener: tokio::net::TcpListener,
}

pub struct Accepted {
    pub stream: tokio::net::TcpStream,
    pub local_addr: SocketAddr,
    pub remote_addr: SocketAddr,
}

impl TcpListener {
    pub async fn bind(addr: SocketAddr) -> io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(addr).await?;

        Ok(Self { listener, addr })
    }

    pub async fn accept(&self) -> io::Result<Accepted> {
        self.listener
            .accept()
            .await
            .map(|(stream, remote_addr)| Accepted {
                stream,
                local_addr: self.addr,
                remote_addr,
            })
    }
}

pub struct Listeners {
    listeners: Vec<TcpListener>,
}

impl Listeners {
    pub async fn new(addrs: &[SocketAddr]) -> io::Result<Self> {
        let mut errs = String::new();
        let mut listeners = Vec::new();

        let mut bind_futs = addrs
            .iter()
            .map(|addr| TcpListener::bind(*addr))
            .collect::<FuturesUnordered<_>>();

        while let Some(bind_res) = bind_futs.next().await {
            match bind_res {
                Ok(listener) => listeners.push(listener),
                Err(err) => {
                    errs.push_str(&err.to_string());
                    errs.push('\n');
                }
            }
        }

        if !errs.is_empty() {
            return Err(io::Error::other(errs));
        }

        Ok(Self { listeners })
    }

    pub fn addrs(&self) -> impl Iterator<Item = SocketAddr> {
        self.listeners.iter().map(|l| l.addr)
    }

    pub async fn accept(&self) -> Accepted {
        let mut futures = FuturesUnordered::new();

        loop {
            futures.clear();

            for listener in &self.listeners {
                futures.push(listener.accept());
            }

            while let Some(res) = futures.next().await {
                match res {
                    Ok(accepted) => return accepted,
                    Err(err) => {
                        tracing::error!("accept error: {err:?}");
                    }
                }
            }
        }
    }
}
