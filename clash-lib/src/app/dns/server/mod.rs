use hickory_proto::op::Message;

use tracing::{error, info, instrument};
use watfaq_dns::DNSListenAddr;

use crate::runner::Runner;

use super::ThreadSafeDNSResolver;

mod handler;
pub use handler::exchange_with_resolver;

static DEFAULT_DNS_SERVER_TTL: u32 = 60;

struct DnsMessageExchanger {
    resolver: ThreadSafeDNSResolver,
}

impl watfaq_dns::DnsMessageExchanger for DnsMessageExchanger {
    fn ipv6(&self) -> bool {
        self.resolver.ipv6()
    }

    #[instrument(skip(self))]
    async fn exchange(
        &self,
        message: &Message,
    ) -> Result<Message, watfaq_dns::DNSError> {
        exchange_with_resolver(&self.resolver, message, true).await
    }
}

pub struct DnsRunner {
    task_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    enable: bool,
    listener: DNSListenAddr,
    resolver: ThreadSafeDNSResolver,
    cwd: std::path::PathBuf,

    cancellation_token: tokio_util::sync::CancellationToken,
}

impl DnsRunner {
    pub fn new(
        enable: bool,
        listen: DNSListenAddr,
        resolver: ThreadSafeDNSResolver,
        cwd: &std::path::Path,
        cancellation_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Self {
        Self {
            task_handle: std::sync::Mutex::new(None),
            enable,
            listener: listen,
            resolver,
            cwd: cwd.to_path_buf(),
            cancellation_token: cancellation_token.unwrap_or_default(),
        }
    }
}

impl Runner for DnsRunner {
    fn run_async(&self) {
        if !self.enable {
            info!("dns listener is disabled, skipping");
            return;
        }

        let resolver = self.resolver.clone();
        let listen = self.listener.clone();
        let cwd = self.cwd.clone();
        let cancellation_token = self.cancellation_token.clone();

        let handle = tokio::spawn(async move {
            let h = DnsMessageExchanger { resolver };
            let r = watfaq_dns::get_dns_listener_with_shutdown(
                listen,
                h,
                &cwd,
                cancellation_token.cancelled_owned(),
            )
            .await;
            if let Some(r) = r {
                if let Err(err) = r.await {
                    error!("dns listener error: {}", err);
                }
                info!("dns listener is closed");
            } else {
                info!("dns listener: no listen addresses configured, skipping");
            }
        });
        *self.task_handle.lock().unwrap() = Some(handle);
    }

    fn shutdown(&self) {
        info!("Shutting down DNS server");
        self.cancellation_token.cancel();
    }

    fn join(&self) -> futures::future::BoxFuture<'_, Result<(), crate::Error>> {
        Box::pin(async move {
            let handle = self.task_handle.lock().unwrap().take();
            if let Some(handle) = handle {
                handle
                    .await
                    .map_err(|e| crate::Error::Operation(e.to_string()))?;
            }
            Ok(())
        })
    }
}
