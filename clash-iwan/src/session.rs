use crate::{
    config,
    core::{auth, crypto, gcm, netstack::*, protocol},
    APP_SECRET,
};
use anyhow::{Context, Result};
use smoltcp::{
    iface::{Config, Interface, SocketHandle, SocketSet},
    socket::tcp,
    time::Instant as SmolInstant,
    wire::{HardwareAddress, IpAddress, IpCidr, IpEndpoint},
};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, UdpSocket},
    pin::Pin,
    sync::mpsc,
    task::{Context as TaskContext, Poll, Waker},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf},
    sync::oneshot,
};

#[derive(Clone, Debug)]
pub struct Options {
    pub config_dir: String,
    pub line: String,
    pub bind: Option<String>,
    pub mtu: u16,
    pub encryption: u8,
}
impl Options {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.config_dir.trim().is_empty(),
            "iwan config-dir is empty"
        );
        anyhow::ensure!(!self.line.trim().is_empty(), "iwan line is empty");
        anyhow::ensure!(
            (576..=2040).contains(&self.mtu),
            "iwan mtu must be 576..=2040"
        );
        anyhow::ensure!(self.encryption <= 1, "iwan supports encryption 0 or 1");
        Ok(())
    }
}

pub fn select_server<'a>(
    servers: &'a [config::Server],
    line: &str,
) -> Result<&'a config::Server> {
    anyhow::ensure!(!line.trim().is_empty(), "empty line selector");
    if let Ok(index) = line.parse::<usize>() {
        return index
            .checked_sub(1)
            .and_then(|i| servers.get(i))
            .context("line index out of range");
    }
    if let Some(server) = servers.iter().find(|s| s.name == line) {
        return Ok(server);
    }
    let mut matches = servers.iter().filter(|s| s.name.contains(line));
    let server = matches.next().context("no matching iwan line")?;
    anyhow::ensure!(
        matches.next().is_none(),
        "ambiguous iwan line; use an exact name or index"
    );
    Ok(server)
}

struct Request {
    ip: Ipv4Addr,
    port: u16,
    stream: DuplexStream,
    ready: oneshot::Sender<std::io::Result<()>>,
}
#[derive(Debug)]
pub struct Session {
    tx: mpsc::SyncSender<Request>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Session {
    pub fn is_closed(&self) -> bool {
        !self.alive.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn start(opts: Options) -> Result<Self> {
        opts.validate()?;
        let cfg = config::load_config(
            &config::resolve_dir(&opts.config_dir).join("servers.json"),
        )?;
        let srv = select_server(&cfg.servers, &opts.line)?;
        anyhow::ensure!(
            !srv.username.is_empty() && srv.username.len() <= 253 && srv.port != 0,
            "invalid iwan line credentials"
        );
        let password = gcm::decrypt_password(
            &srv.password,
            APP_SECRET,
            &cfg.domain,
            &srv.username,
        )
        .context("decrypt iwan credential")?;
        let nonce = auth::rand_u32()?;
        let open = auth::build_open(
            &srv.username,
            &auth::get_ct(&srv.username, &password, None)?,
            opts.mtu,
            opts.encryption,
            nonce,
        );
        let sock =
            auth::udp_connect(&srv.host, srv.port, 3000, opts.bind.as_deref())?;
        let mut ack = None;
        for _ in 0..3 {
            sock.send(&open)?;
            let mut buf = [0; 4096];
            if let Ok(n) = sock.recv(&mut buf) {
                ack = Some(auth::parse_ack(&buf[..n], nonce)?);
                break;
            }
        }
        let ack = ack.context("iwan authentication timed out")?;
        anyhow::ensure!(ack.mtu >= 576, "invalid negotiated iwan MTU");
        let key = crypto::session_key(&srv.username, &password)[..8].to_vec();
        let ip: Ipv4Addr = ack.tun.parse().context("invalid tunnel IP")?;
        let gateway: Ipv4Addr = ack.gw.parse().context("invalid tunnel gateway")?;
        sock.set_nonblocking(true)?;
        let (tx, rx) = mpsc::sync_channel(64);
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let running = alive.clone();
        std::thread::Builder::new()
            .name("iwan".into())
            .spawn(move || {
                let _running = Alive(running);
                let _close = Close {
                    sock: &sock,
                    sid: ack.sid,
                    token: ack.tok,
                    encryption: opts.encryption,
                };
                if let Err(error) = run(&sock, &ack, &key, &opts, ip, gateway, rx) {
                    eprintln!("iwan session ended: {error:#}");
                }
            })?;
        Ok(Self { tx, alive })
    }
    pub async fn connect(
        &self,
        ip: Ipv4Addr,
        port: u16,
    ) -> std::io::Result<DuplexStream> {
        let (client, stream) = tokio::io::duplex(64 * 1024);
        let (ready, wait) = oneshot::channel();
        self.tx
            .try_send(Request {
                ip,
                port,
                stream,
                ready,
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        tokio::time::timeout(Duration::from_secs(30), wait)
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "iwan TCP connect timed out",
                )
            })?
            .map_err(|_| std::io::Error::other("iwan session closed"))??;
        Ok(client)
    }
}
struct Close<'a> {
    sock: &'a UdpSocket,
    sid: u16,
    token: u32,
    encryption: u8,
}
impl Drop for Close<'_> {
    fn drop(&mut self) {
        let _ = self.sock.send(&protocol::ctrl_pkt(
            &protocol::pkhdr(
                protocol::PT_CLOSE,
                self.encryption,
                self.sid,
                self.token,
            ),
            &[],
        ));
    }
}
struct Flow {
    stream: DuplexStream,
    ready: Option<oneshot::Sender<std::io::Result<()>>>,
    started: Instant,
    write_closed: bool,
}

#[allow(clippy::too_many_arguments)]
fn run(
    sock: &UdpSocket,
    ack: &auth::AuthResult,
    key: &[u8],
    opts: &Options,
    ip: Ipv4Addr,
    gateway: Ipv4Addr,
    rx: mpsc::Receiver<Request>,
) -> Result<()> {
    let started = Instant::now();
    let now = || {
        SmolInstant::from_millis(
            started.elapsed().as_millis().min(i64::MAX as u128) as i64
        )
    };
    let mtu = ack.mtu.min(opts.mtu) as usize;
    let mut device = IpTunnelDevice::new(mtu);
    let mut conf = Config::new(HardwareAddress::Ip);
    conf.random_seed = rand::random();
    let mut iface = Interface::new(conf, &mut device, now());
    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(IpAddress::Ipv4(ip), 32)).unwrap();
    });
    iface
        .routes_mut()
        .add_default_ipv4_route(gateway)
        .map_err(|_| anyhow::anyhow!("cannot add iwan default route"))?;
    let mut sockets = SocketSet::new(Vec::new());
    let mut flows: HashMap<u16, (SocketHandle, Flow)> = HashMap::new();
    let mut keepalive = Instant::now();
    let mut last_received = Instant::now();
    let mut cx = TaskContext::from_waker(Waker::noop());
    loop {
        for _ in 0..64 {
            let req = match rx.try_recv() {
                Ok(req) => req,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            };
            if req.ready.is_closed() {
                continue;
            }
            let Some(port) = (49152..=65535).find(|p| !flows.contains_key(p)) else {
                let _ = req
                    .ready
                    .send(Err(std::io::Error::other("iwan ports exhausted")));
                continue;
            };
            let mut tcp = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; 65536]),
                tcp::SocketBuffer::new(vec![0; 65536]),
            );
            tcp.set_timeout(Some(smoltcp::time::Duration::from_secs(120)));
            if tcp
                .connect(
                    iface.context(),
                    IpEndpoint::new(IpAddress::Ipv4(req.ip), req.port),
                    port,
                )
                .is_err()
            {
                let _ = req.ready.send(Err(std::io::Error::other(
                    "invalid iwan TCP destination",
                )));
                continue;
            }
            flows.insert(
                port,
                (
                    sockets.add(tcp),
                    Flow {
                        stream: req.stream,
                        ready: Some(req.ready),
                        started: Instant::now(),
                        write_closed: false,
                    },
                ),
            );
        }
        if receive_vpn(
            sock,
            &mut device,
            key,
            ack.sid,
            ack.tok,
            mtu,
            opts.encryption,
            started,
        )? {
            last_received = Instant::now();
        }
        anyhow::ensure!(
            last_received.elapsed() < Duration::from_secs(60),
            "iwan session timed out"
        );
        iface.poll(now(), &mut device, &mut sockets);
        flows.retain(|_, (handle, flow)| {
            let tcp = sockets.get_mut::<tcp::Socket>(*handle);
            if let Some(ready) = &flow.ready {
                if ready.is_closed()
                    || flow.started.elapsed() >= Duration::from_secs(30)
                {
                    tcp.abort();
                }
                if tcp.state() == tcp::State::Established {
                    let _ = flow.ready.take().unwrap().send(Ok(()));
                }
            }
            if flow.ready.is_none() {
                let mut bytes = [0; 16384];
                let capacity = tcp
                    .send_capacity()
                    .saturating_sub(tcp.send_queue())
                    .min(bytes.len());
                if tcp.may_send() && capacity > 0 {
                    let mut buf = ReadBuf::new(&mut bytes[..capacity]);
                    match Pin::new(&mut flow.stream).poll_read(&mut cx, &mut buf) {
                        Poll::Ready(Ok(())) if buf.filled().is_empty() => {
                            tcp.close()
                        }
                        Poll::Ready(Ok(())) => {
                            let _ = tcp.send_slice(buf.filled());
                        }
                        Poll::Ready(Err(_)) => tcp.abort(),
                        Poll::Pending => {}
                    }
                }
                let mut local_error = false;
                if tcp.can_recv() {
                    let _ = tcp.recv(|data| {
                        match Pin::new(&mut flow.stream).poll_write(&mut cx, data) {
                            Poll::Ready(Ok(n)) => (n, ()),
                            Poll::Ready(Err(_)) => {
                                local_error = true;
                                (0, ())
                            }
                            Poll::Pending => (0, ()),
                        }
                    });
                }
                if local_error {
                    tcp.abort();
                }
                if !tcp.may_recv()
                    && !tcp.can_recv()
                    && !flow.write_closed
                    && Pin::new(&mut flow.stream).poll_shutdown(&mut cx).is_ready()
                {
                    flow.write_closed = true;
                }
            }
            if tcp.state() == tcp::State::Closed {
                if let Some(ready) = flow.ready.take() {
                    let _ = ready.send(Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "iwan TCP connection failed",
                    )));
                }
                sockets.remove(*handle);
                false
            } else {
                true
            }
        });
        iface.poll(now(), &mut device, &mut sockets);
        send_vpn(sock, &mut device, key, ack.sid, ack.tok, opts.encryption)?;
        send_vpn_keepalive(sock, ack.sid, ack.tok, opts.encryption, &mut keepalive)?;
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Alive(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for Alive {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn selects_lines_without_silent_ambiguity() {
        let servers: Vec<config::Server> =
            serde_json::from_value(serde_json::json!([
                {"name":"教育网一","host":"127.0.0.1"},
                {"name":"教育网二","host":"127.0.0.2"}
            ]))
            .unwrap();
        assert_eq!(select_server(&servers, "2").unwrap().host, "127.0.0.2");
        assert_eq!(
            select_server(&servers, "教育网一").unwrap().host,
            "127.0.0.1"
        );
        for invalid in ["0", "3", "", "教育网", "missing"] {
            assert!(select_server(&servers, invalid).is_err());
        }
    }

    #[tokio::test]
    async fn tunnel_tcp_echo_backpressure_half_close_and_shutdown() {
        for encryption in [0, 1] {
            let client = UdpSocket::bind("127.0.0.1:0").unwrap();
            let server = UdpSocket::bind("127.0.0.1:0").unwrap();
            client.connect(server.local_addr().unwrap()).unwrap();
            server.connect(client.local_addr().unwrap()).unwrap();
            client.set_nonblocking(true).unwrap();
            server.set_nonblocking(true).unwrap();
            let ack = auth::AuthResult {
                sid: 7,
                tok: 99,
                tun: "10.0.0.2".into(),
                gw: "10.0.0.1".into(),
                dns: "10.0.0.1".into(),
                mtu: 1380,
            };
            let opts = Options {
                config_dir: "unused".into(),
                line: "1".into(),
                bind: None,
                mtu: 1380,
                encryption,
            };
            let (tx, rx) = mpsc::sync_channel(64);
            let alive =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let session = Session { tx, alive };
            let worker = std::thread::spawn(move || {
                run(
                    &client,
                    &ack,
                    &[42; 8],
                    &opts,
                    "10.0.0.2".parse().unwrap(),
                    "10.0.0.1".parse().unwrap(),
                    rx,
                )
            });
            let stop =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let done = stop.clone();
            let peer = std::thread::spawn(move || {
                let started = Instant::now();
                let mut device = IpTunnelDevice::new(1380);
                let mut iface = Interface::new(
                    Config::new(HardwareAddress::Ip),
                    &mut device,
                    SmolInstant::from_millis(0),
                );
                iface.update_ip_addrs(|ips| {
                    ips.push(IpCidr::new(
                        IpAddress::Ipv4("10.0.0.1".parse().unwrap()),
                        24,
                    ))
                    .unwrap();
                });
                let mut sockets = SocketSet::new(Vec::new());
                for port in [80, 81] {
                    let mut tcp = tcp::Socket::new(
                        tcp::SocketBuffer::new(vec![0; 32768]),
                        tcp::SocketBuffer::new(vec![0; 32768]),
                    );
                    tcp.listen(port).unwrap();
                    sockets.add(tcp);
                }
                while !done.load(std::sync::atomic::Ordering::Relaxed)
                    && started.elapsed() < Duration::from_secs(20)
                {
                    receive_vpn(
                        &server,
                        &mut device,
                        &[42; 8],
                        7,
                        99,
                        1380,
                        encryption,
                        started,
                    )
                    .unwrap();
                    iface.poll(
                        SmolInstant::from_millis(
                            started.elapsed().as_millis() as i64
                        ),
                        &mut device,
                        &mut sockets,
                    );
                    for (_, socket) in sockets.iter_mut() {
                        let smoltcp::socket::Socket::Tcp(tcp) = socket;
                        let room =
                            tcp.send_capacity().saturating_sub(tcp.send_queue());
                        if tcp.can_recv() && room > 0 {
                            let mut data = vec![0; room.min(16384)];
                            let n = tcp.recv_slice(&mut data).unwrap();
                            assert_eq!(tcp.send_slice(&data[..n]).unwrap(), n);
                        }
                        if tcp.state() == tcp::State::CloseWait && !tcp.can_recv() {
                            tcp.close();
                        }
                    }
                    iface.poll(
                        SmolInstant::from_millis(
                            started.elapsed().as_millis() as i64
                        ),
                        &mut device,
                        &mut sockets,
                    );
                    send_vpn(&server, &mut device, &[42; 8], 7, 99, encryption)
                        .unwrap();
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            let result = tokio::time::timeout(Duration::from_secs(15), async {
                let (first, second) = tokio::join!(
                    session.connect("10.0.0.1".parse().unwrap(), 80),
                    session.connect("10.0.0.1".parse().unwrap(), 81)
                );
                async fn echo(stream: DuplexStream, byte: u8) {
                    let (mut read, mut write) = tokio::io::split(stream);
                    let expected = vec![byte; 256 * 1024];
                    let sent = expected.clone();
                    let sender = tokio::spawn(async move {
                        write.write_all(&sent).await.unwrap();
                        write.shutdown().await.unwrap();
                    });
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let mut received = Vec::new();
                    read.read_to_end(&mut received).await.unwrap();
                    sender.await.unwrap();
                    assert_eq!(received, expected);
                }
                tokio::join!(
                    echo(first.unwrap(), 0x21),
                    echo(second.unwrap(), 0x42)
                );
            })
            .await;
            drop(session);
            worker.join().unwrap().unwrap();
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            peer.join().unwrap();
            result.unwrap();
        }
    }
}
