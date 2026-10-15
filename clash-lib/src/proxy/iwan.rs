use super::{
    ConnectorType, DialWithConnector, OutboundHandler, OutboundType,
    PlainProxyAPIResponse,
};
use crate::{
    app::{
        dispatcher::{
            BoxedInstrumentedDatagram, BoxedInstrumentedStream, InstrumentedStream,
            InstrumentedStreamWrapper,
        },
        dns::ThreadSafeDNSResolver,
    },
    config::internal::proxy::OutboundIwan,
    session::Session,
};
use async_trait::async_trait;
use std::{collections::HashMap, net::IpAddr, sync::Arc};
use tokio::sync::Mutex;

#[derive(Debug)]
pub struct Handler {
    opts: OutboundIwan,
    session: Mutex<Option<Arc<clash_iwan::session::Session>>>,
}
impl Handler {
    pub fn new(opts: OutboundIwan) -> Self {
        Self {
            opts,
            session: Mutex::new(None),
        }
    }
}
impl DialWithConnector for Handler {}
#[async_trait]
impl OutboundHandler for Handler {
    fn name(&self) -> &str {
        &self.opts.name
    }
    fn proto(&self) -> OutboundType {
        OutboundType::Iwan
    }
    async fn support_udp(&self) -> bool {
        false
    }
    async fn support_connector(&self) -> ConnectorType {
        ConnectorType::None
    }
    async fn connect_stream(
        &self,
        sess: &Session,
        resolver: ThreadSafeDNSResolver,
    ) -> std::io::Result<BoxedInstrumentedStream> {
        let ip = match sess.destination.ip() {
            Some(ip) => ip,
            None => resolver
                .resolve(&sess.destination.host(), false)
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?
                .ok_or_else(|| {
                    std::io::Error::other("iwan DNS returned no address")
                })?,
        };
        let IpAddr::V4(ip) = ip else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "iwan supports IPv4 destinations only",
            ));
        };
        let session = {
            let mut guard = self.session.lock().await;
            if guard.as_ref().is_none_or(|s| s.is_closed()) {
                let opts = clash_iwan::session::Options {
                    config_dir: self.opts.config_dir.clone(),
                    line: self.opts.line.clone(),
                    bind: self.opts.bind.clone(),
                    mtu: self.opts.mtu,
                    encryption: self.opts.encryption,
                };
                let session = tokio::task::spawn_blocking(move || {
                    clash_iwan::session::Session::start(opts)
                })
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?
                .map_err(|e| std::io::Error::other(format!("iwan: {e:#}")))?;
                *guard = Some(Arc::new(session));
            }
            guard.as_ref().unwrap().clone()
        };
        let stream = InstrumentedStreamWrapper::new(
            session.connect(ip, sess.destination.port()).await?,
        );
        stream.append_to_chain(self.name()).await;
        Ok(Box::new(stream))
    }
    async fn connect_datagram(
        &self,
        _: &Session,
        _: ThreadSafeDNSResolver,
    ) -> std::io::Result<BoxedInstrumentedDatagram> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "iwan does not support UDP relay",
        ))
    }
    fn try_as_plain_handler(&self) -> Option<&dyn PlainProxyAPIResponse> {
        Some(self)
    }
}
#[async_trait]
impl PlainProxyAPIResponse for Handler {
    async fn as_map(
        &self,
    ) -> HashMap<String, Box<dyn erased_serde::Serialize + Send>> {
        HashMap::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::internal::proxy::OutboundProxyProtocol;
    #[tokio::test]
    async fn config_registers_native_tcp_only_handler() {
        let config: OutboundProxyProtocol = serde_yaml::from_str(
            "type: iwan\nname: campus\nconfig-dir: /tmp/credentials\nline: 教育网\n",
        )
        .unwrap();
        assert_eq!(config.name(), "campus");
        let OutboundProxyProtocol::Iwan(opts) = config else {
            panic!("wrong protocol")
        };
        assert_eq!(opts.mtu, 1380);
        assert_eq!(opts.encryption, 1);
        let handler = Handler::new(opts);
        assert!(!handler.support_udp().await);
        assert_eq!(handler.proto().to_string(), "iWAN");
    }
}
