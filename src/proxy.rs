use std::{
    cell::Cell,
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use iroh::endpoint::{RecvStream, SendStream};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::Instant,
};

/// Max time a proxied conn can go without data moving in either direction
/// before it gets torn down
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Number of streams currently being proxied. Useful for spotting leaks
/// before hitting the stream limit
#[derive(Clone, Default)]
pub struct ActiveStreams(Arc<AtomicUsize>);

impl ActiveStreams {
    /// Counts a stream as active until the returned guard is dropped
    pub fn track(&self) -> ActiveStreamGuard {
        self.0.fetch_add(1, Ordering::Relaxed);
        ActiveStreamGuard(self.0.clone())
    }

    pub fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

pub struct ActiveStreamGuard(Arc<AtomicUsize>);

impl Drop for ActiveStreamGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Enables TCP keepalive so peers that vanish without a FIN/RST
/// (dead NAT mappings, devices dropping off the network) get noticed by the OS
pub fn set_keepalive(stream: &TcpStream) {
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(15));

    if let Err(err) = socket2::SockRef::from(stream).set_tcp_keepalive(&keepalive) {
        tracing::warn!("Failed to set TCP keepalive: {err:?}");
    }
}

/// Copies data both ways between `tcp` and the iroh stream until both directions
/// hit EOF. Half-closes are passed through, so EOF in one direction doesn't cut
/// off the other.
///
/// On error or idle timeout the iroh stream is reset/stopped so its slot
/// in the stream limit is freed right away.
pub async fn proxy(
    tcp: &mut TcpStream,
    send: &mut SendStream,
    recv: &mut RecvStream,
) -> io::Result<()> {
    let (mut tcp_recv, mut tcp_send) = tcp.split();
    let last_activity = Cell::new(Instant::now());

    let res = tokio::select! {
        res = async {
            tokio::try_join!(
                pipe(recv, &mut tcp_send, &last_activity),
                pipe(&mut tcp_recv, send, &last_activity),
            )
        } => res.map(|_| ()),
        _ = idle_timeout(&last_activity) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "no data sent for too long",
        )),
    };

    if res.is_err() {
        send.reset(0u32.into()).ok();
        recv.stop(0u32.into()).ok();
    }

    res
}

/// Copies `reader` into `writer` until EOF, then shuts down `writer`
/// to forward the half-close (TCP FIN / iroh stream finish)
async fn pipe<R, W>(reader: &mut R, writer: &mut W, last_activity: &Cell<Instant>) -> io::Result<()>
where
    R: AsyncRead + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
{
    let mut buf = vec![0u8; 16 * 1024];

    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        writer.write_all(&buf[..n]).await?;
        last_activity.set(Instant::now());
    }

    writer.shutdown().await
}

/// Resolves once `last_activity` is more than `IDLE_TIMEOUT` ago
async fn idle_timeout(last_activity: &Cell<Instant>) {
    loop {
        let deadline = last_activity.get() + IDLE_TIMEOUT;
        if Instant::now() >= deadline {
            return;
        }

        tokio::time::sleep_until(deadline).await;
    }
}
