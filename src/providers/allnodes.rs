use {
    super::{
        is_internal_error_rpc_code, is_node_error_rpc_message, is_rate_limited_error_rpc_message,
        Provider, ProviderKind, RateLimited, RpcProvider, RpcProviderFactory, RpcQueryParams,
        RpcWsProvider, WS_PROXY_TASK_METRICS,
    },
    crate::{
        env::AllnodesConfig,
        error::{RpcError, RpcResult},
        ws,
    },
    async_trait::async_trait,
    axum::{
        http::HeaderValue,
        response::{IntoResponse, Response},
    },
    axum_tungstenite::WebSocketUpgrade,
    hyper::{client::HttpConnector, http, Client, Method},
    hyper_tls::HttpsConnector,
    std::collections::HashMap,
    tracing::debug,
    wc::future::FutureExt,
};

#[derive(Debug)]
pub struct AllnodesProvider {
    pub client: Client<HttpsConnector<HttpConnector>>,
    pub supported_chains: HashMap<String, String>,
    pub api_key: String,
}

#[derive(Debug)]
pub struct AllnodesWsProvider {
    pub supported_chains: HashMap<String, String>,
    pub api_key: String,
}

impl Provider for AllnodesWsProvider {
    fn supports_caip_chainid(&self, chain_id: &str) -> bool {
        self.supported_chains.contains_key(chain_id)
    }

    fn supported_caip_chains(&self) -> Vec<String> {
        self.supported_chains.keys().cloned().collect()
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Allnodes
    }
}

#[async_trait]
impl RpcWsProvider for AllnodesWsProvider {
    #[tracing::instrument(skip_all, fields(provider = %self.provider_kind()), level = "debug")]
    async fn proxy(
        &self,
        ws: WebSocketUpgrade,
        query_params: RpcQueryParams,
    ) -> RpcResult<Response> {
        let chain = &self
            .supported_chains
            .get(&query_params.chain_id)
            .ok_or(RpcError::ChainNotFound)?;

        let project_id = query_params.project_id;
        let uri = format!("wss://{}.allnodes.me:8546/{}", chain, &self.api_key);
        let (websocket_provider, _) = async_tungstenite::tokio::connect_async(uri)
            .await
            .map_err(|e| RpcError::AxumTungstenite(Box::new(e)))?;

        Ok(ws.on_upgrade(move |socket| {
            ws::proxy(project_id, socket, websocket_provider)
                .with_metrics(WS_PROXY_TASK_METRICS.with_name("allnodes"))
        }))
    }
}

#[async_trait]
impl RateLimited for AllnodesWsProvider {
    async fn is_rate_limited(&self, response: &mut Response) -> bool
    where
        Self: Sized,
    {
        response.status() == http::StatusCode::TOO_MANY_REQUESTS
    }
}

impl Provider for AllnodesProvider {
    fn supports_caip_chainid(&self, chain_id: &str) -> bool {
        self.supported_chains.contains_key(chain_id)
    }

    fn supported_caip_chains(&self) -> Vec<String> {
        self.supported_chains.keys().cloned().collect()
    }

    fn provider_kind(&self) -> ProviderKind {
        ProviderKind::Allnodes
    }
}

#[async_trait]
impl RateLimited for AllnodesProvider {
    async fn is_rate_limited(&self, response: &mut Response) -> bool {
        response.status() == http::StatusCode::TOO_MANY_REQUESTS
    }
}

#[async_trait]
impl RpcProvider for AllnodesProvider {
    #[tracing::instrument(skip(self, body), fields(provider = %self.provider_kind()), level = "debug")]
    async fn proxy(&self, chain_id: &str, body: hyper::body::Bytes) -> RpcResult<Response> {
        let chain = &self
            .supported_chains
            .get(chain_id)
            .ok_or(RpcError::ChainNotFound)?;

        let uri = format!("https://{}.allnodes.me:8545/{}", chain, &self.api_key);

        let hyper_request = hyper::http::Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("Content-Type", "application/json")
            .body(hyper::body::Body::from(body))?;

        let response = self.client.request(hyper_request).await?;
        let status = response.status();
        let body = hyper::body::to_bytes(response.into_body()).await?;

        if status.is_success() {
            if let Ok(json_response) = serde_json::from_slice::<jsonrpc::Response>(&body) {
                if let Some(error) = &json_response.error {
                    debug!(
                        "Strange: provider returned JSON RPC error, but status {status} is success: \
                     Allnodes: {json_response:?}"
                    );
                    if is_internal_error_rpc_code(error.code) {
                        if is_rate_limited_error_rpc_message(&error.message) {
                            return Ok((http::StatusCode::TOO_MANY_REQUESTS, body).into_response());
                        }
                        if is_node_error_rpc_message(&error.message) {
                            return Ok(
                                (http::StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
                            );
                        }
                    }
                }
            }
        }

        let mut response = (status, body).into_response();
        response
            .headers_mut()
            .insert("Content-Type", HeaderValue::from_static("application/json"));
        Ok(response)
    }
}

impl RpcProviderFactory<AllnodesConfig> for AllnodesProvider {
    #[tracing::instrument(level = "debug")]
    fn new(provider_config: &AllnodesConfig) -> Self {
        let forward_proxy_client = Client::builder().build::<_, hyper::Body>(HttpsConnector::new());
        let supported_chains: HashMap<String, String> = provider_config
            .supported_chains
            .iter()
            .map(|(k, v)| (k.clone(), v.0.clone()))
            .collect();

        AllnodesProvider {
            client: forward_proxy_client,
            supported_chains,
            api_key: provider_config.api_key.clone(),
        }
    }
}

impl RpcProviderFactory<AllnodesConfig> for AllnodesWsProvider {
    #[tracing::instrument(level = "debug")]
    fn new(provider_config: &AllnodesConfig) -> Self {
        let supported_chains: HashMap<String, String> = provider_config
            .supported_ws_chains
            .iter()
            .map(|(k, v)| (k.clone(), v.0.clone()))
            .collect();

        AllnodesWsProvider {
            supported_chains,
            api_key: provider_config.api_key.clone(),
        }
    }
}
