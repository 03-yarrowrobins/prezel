use std::net::{Ipv4Addr, SocketAddrV4};

use async_trait::async_trait;
use cookie::Cookie;
use http::{header, Response, StatusCode};
use hyper::body::Bytes;
use pingora::apps::http_app::ServeHttp;
use pingora::http::ResponseHeader;
use pingora::listeners::{TlsAccept, TlsSettings};
use pingora::prelude::http_proxy_service;
use pingora::prelude::{HttpPeer, ProxyHttp, Result, Session};
use pingora::protocols::http::ServerSession;
use pingora::server::Server;
use pingora::services::listening::Service;
use pingora::tls::{self, ssl};
use pingora::ErrorType::Custom;
use pingora::{Error, ErrorSource};
use url::Url;

use crate::api::API_PORT;
use crate::conf::Conf;
use crate::deployments::manager::Manager;
use crate::listener::{Access, Listener};
use crate::logging::{Level, RequestLog, RequestLogger};
use crate::time::now;
use crate::tls::certificate::TlsCertificate;

struct ApiListener;

// TODO: move this to api mod
#[async_trait]
impl Listener for ApiListener {
    async fn access(&self) -> anyhow::Result<Access> {
        Ok(SocketAddrV4::new(Ipv4Addr::LOCALHOST, API_PORT).into())
    }
    fn is_public(&self) -> bool {
        true
    }
}

struct Peer {
    listener: Box<dyn Listener>,
    deployment_id: Option<i64>,
}

impl<L: Listener + 'static> From<L> for Peer {
    fn from(value: L) -> Self {
        Peer {
            listener: Box::new(value),
            deployment_id: None,
        }
    }
}

struct ProxyApp {
    manager: Manager,
    config: Conf,
    request_logger: RequestLogger,
}

impl ProxyApp {
    async fn get_listener_inner(&self, session: &Session) -> Option<Peer> {
        // TODO: try to use session.req_header().uri.host()
        let host = session.get_header(header::HOST)?.to_str().ok()?;

        if host == self.config.api_hostname() {
            Some(ApiListener.into())
        } else {
            let container = self.manager.get_container_by_hostname(host).await?;
            let deployment_id = container.logging_deployment_id.clone();
            Some(Peer {
                listener: Box::new(container),
                deployment_id,
            })
        }
    }

    async fn get_listener(&self, session: &Session) -> Result<Peer, Box<Error>> {
        self.get_listener_inner(session)
            .await
            .ok_or(Error::new_str("No peer found"))
    }

    fn is_authenticated(&self, session: &Session) -> bool {
        let hostname = &self.config.hostname;
        session
            .get_header(header::COOKIE)
            .and_then(|header| header.to_str().ok())
            .and_then(|cookie_header| {
                Cookie::split_parse(cookie_header)
                    .filter_map(|cookie| cookie.ok())
                    .find(|cookie| cookie.name() == hostname && cookie.value() == self.config.token)
            })
            .is_some()
    }
}

#[derive(Default)]
struct RequestCtx {
    deployment: Option<i64>,
    socket: Option<SocketAddrV4>,
}

#[async_trait]
impl ProxyHttp for ProxyApp {
    type CTX = RequestCtx;
    fn new_ctx(&self) -> Self::CTX {
        Default::default()
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let socket = ctx
            .socket
            .ok_or_else(|| Error::new_str("illegal upstream_peer call with empty socket"))?;
        let proxy_to = HttpPeer::new(socket, false, "".to_owned());
        let peer = Box::new(proxy_to);
        Ok(peer)
    }

    // I never simply return true, so maybe I could simply do the redirect from inside upstream_peer?
    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let Peer {
            listener,
            deployment_id,
        } = self.get_listener(session).await?;
        ctx.deployment = deployment_id;

        // let listener = self.get_listener(session).await?.listener;
        if listener.is_public() || self.is_authenticated(session) {
            let access = listener.access().await.map_err(|error| {
                Error::create(
                    Custom("Failed to aquire socket"),
                    ErrorSource::Unset, // FIXME: is this correct ??
                    None,
                    Some(error.into()),
                )
            })?;
            match access {
                Access::Socket(socket) => {
                    ctx.socket = Some(socket);
                    Ok(false)
                }
                Access::Loading => {
                    let code = StatusCode::OK;
                    let mut resp: Box<_> = ResponseHeader::build(code, None)?.into();
                    resp.insert_header("Prezel-Loading", "true")?;
                    session.set_keepalive(None); // TODO: review this?
                    session.write_response_header(resp, false).await?;
                    session
                        .write_response_body(
                            Some(Bytes::from_static(include_bytes!(
                                "../resources/loading.html"
                            ))),
                            true,
                        )
                        .await?;
                    Ok(true)
                }
            }
        } else {
            let host = session.get_header(header::HOST).unwrap().to_str().unwrap();
            let path = session.req_header().uri.path();
            let callback = Url::parse(&format!("https://{host}{path}")).unwrap();

            let coordinator = &self.config.coordinator;
            let mut redirect = Url::parse(&format!("{coordinator}/api/instance/auth")).unwrap();
            redirect
                .query_pairs_mut()
                .append_pair("callback", callback.as_str());

            let code = StatusCode::FOUND;
            let mut resp: Box<_> = ResponseHeader::build(code, None)?.into();
            resp.insert_header(header::LOCATION, redirect.as_str())?;
            session.set_keepalive(None); // TODO: review this?
            session.write_response_header(resp, true).await?;
            Ok(true)
        }
    }

    // async fn response_filter(
    //     &self,
    //     _session: &mut Session,
    //     upstream_response: &mut ResponseHeader,
    //     _ctx: &mut Self::CTX,
    // ) -> Result<()>
    // where
    //     Self::CTX: Send + Sync,
    // {
    //     upstream_response
    //         .insert_header("Access-Control-Allow-Origin", &self.config.coordinator)
    //         .unwrap();
    //     Ok(())
    // }

    async fn logging(
        &self,
        session: &mut Session,
        _e: Option<&pingora::Error>,
        ctx: &mut Self::CTX,
    ) {
        logging(session, ctx, &self.request_logger);
    }
}

fn logging(session: &Session, ctx: &RequestCtx, logger: &RequestLogger) -> Option<()> {
    let host = session.get_header(header::HOST)?.to_str().ok()?.to_owned();
    let path = session.req_header().uri.path().to_owned();
    let method = session.req_header().method.as_str().to_owned();
    let deployment = ctx.deployment?;
    let response = session.response_written()?;

    let level = if response.status.is_client_error() || response.status.is_server_error() {
        Level::ERROR
    } else {
        Level::INFO
    };

    logger.log(RequestLog {
        level,
        deployment,
        time: now(),
        host,
        method,
        path,
        status: response.status.as_u16(),
    });

    Some(())
}

struct TlsCallback {
    certificate: TlsCertificate,
}

#[async_trait]
impl TlsAccept for TlsCallback {
    async fn certificate_callback(&self, ssl: &mut ssl::SslRef) {
        tls::ext::ssl_use_certificate(ssl, &self.certificate.cert).unwrap();
        tls::ext::ssl_use_private_key(ssl, &self.certificate.key).unwrap();
    }
}

struct HttpHandler;

#[async_trait]
impl ServeHttp for HttpHandler {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        if let Some(host) = session.req_header().uri.host() {
            // println!("redirecting HTTP query to {host}");
            let path = session.req_header().uri.path();

            let body = "<html><body>301 Moved Permanently</body></html>"
                .as_bytes()
                .to_owned();
            Response::builder()
                .status(StatusCode::MOVED_PERMANENTLY)
                .header(header::CONTENT_TYPE, "text/html")
                .header(header::CONTENT_LENGTH, body.len())
                .header(header::LOCATION, format!("https://{host}{path}"))
                .body(body)
                .unwrap()
        } else {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(vec![]) // FIXME: is this ok? how can I return an empty body?
                .unwrap()
        }
    }
}

pub(crate) fn run_proxy(manager: Manager, config: Conf, certificate: TlsCertificate) {
    let request_logger = RequestLogger::new();
    let mut server = Server::new(None).unwrap();
    server.bootstrap();
    let proxy_app = ProxyApp {
        manager,
        config,
        request_logger,
    };
    let mut https_service = http_proxy_service(&server.configuration, proxy_app);
    let tls_callback = Box::new(TlsCallback { certificate });
    let tls_settings = TlsSettings::with_callbacks(tls_callback).unwrap();
    https_service.add_tls_with_settings("0.0.0.0:443", None, tls_settings);
    server.add_service(https_service);

    let mut http_service = Service::new(
        "HTTP service".to_string(), // TODO: review this name ?
        HttpHandler,
    );
    http_service.add_tcp("0.0.0.0:80");
    server.add_service(http_service);

    server.run_forever();
}
