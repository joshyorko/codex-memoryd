//! Bounded read-only attachment to an existing loopback HTTP daemon.
//! This module has no storage, configuration-file, or daemon lifecycle access.

use std::net::SocketAddr;
use std::time::Duration;

use reqwest::{Client, Method, Url};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};

use crate::domain::Profile;
use crate::error::{Error, ErrorCode, Result};
use crate::protocol::{
    Envelope, RecallRequest, RecallResponse, SearchRequest, SearchResponse, StatusResponse,
};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

pub struct DaemonClient {
    client: Client,
    runtime: tokio::runtime::Runtime,
    endpoint: Url,
    profile: String,
    workspace: String,
}

impl DaemonClient {
    pub fn new(endpoint: &str, profile: &str, workspace: &str) -> Result<Self> {
        let authority = endpoint
            .strip_prefix("http://")
            .ok_or_else(invalid_endpoint)?;
        let authority = authority.strip_suffix('/').unwrap_or(authority);
        let address = authority
            .parse::<SocketAddr>()
            .map_err(|_| invalid_endpoint())?;
        let endpoint = Url::parse(endpoint).map_err(|_| invalid_endpoint())?;
        if endpoint.scheme() != "http"
            || !address.ip().is_loopback()
            || address.port() == 0
            || endpoint
                .port_or_known_default()
                .is_none_or(|port| port == 0)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.path() != "/"
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid_endpoint());
        }
        if Profile::parse(profile).is_none_or(|p| p.as_str() != profile) {
            return Err(Error::invalid_request(
                "daemon profile must be a canonical MemoryD profile",
            ));
        }
        if workspace.is_empty()
            || workspace.len() > 128
            || workspace.trim_matches('-') != workspace
            || !workspace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
        {
            return Err(Error::invalid_request(
                "daemon workspace must be a canonical nonempty identifier of at most 128 bytes",
            ));
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(1))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| Error::internal("failed to construct daemon HTTP client"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::internal("failed to construct daemon client runtime"))?;
        Ok(Self {
            client,
            runtime,
            endpoint,
            profile: profile.to_owned(),
            workspace: workspace.to_owned(),
        })
    }

    pub fn status(&self) -> Result<Value> {
        let status: StatusResponse = self.request(Method::GET, "/v1/status", None)?;
        if status.api_version != crate::API_VERSION || status.provider_name != crate::PROVIDER_NAME
        {
            return Err(Error::new(
                ErrorCode::UnsupportedVersion,
                "daemon status reports an unsupported provider or API version",
            ));
        }
        // Status is daemon-global. Do not disclose other scopes, storage paths,
        // adjacent endpoints, or freeform job/provider diagnostics over MCP.
        Ok(json!({
            "provider_name": status.provider_name,
            "provider_version": status.provider_version,
            "api_version": status.api_version,
            "storage_schema_version": status.storage_schema_version,
            "status": status.status,
            "storage": { "kind": status.storage.kind, "writable": status.storage.writable },
            "attachment": { "mode": "existing_daemon", "profile": self.profile, "workspace": self.workspace, "read_only": true }
        }))
    }

    pub fn recall(&self, mut request: RecallRequest) -> Result<Value> {
        self.bind_scope(&mut request.profile, &mut request.workspace)?;
        let response: RecallResponse =
            self.request(Method::POST, "/v1/recall", Some(encode(&request)?))?;
        if response.facts.iter().any(|fact| {
            fact.policy.provenance.profile_id != self.profile
                || fact.policy.provenance.workspace_id != self.workspace
        }) {
            return Err(Error::profile_boundary(
                "daemon recall response exceeds the configured scope",
            ));
        }
        Ok(json!(response))
    }

    pub fn search(&self, mut request: SearchRequest) -> Result<Value> {
        self.bind_scope(&mut request.profile, &mut request.workspace)?;
        let response: SearchResponse =
            self.request(Method::POST, "/v1/search", Some(encode(&request)?))?;
        if response
            .matches
            .iter()
            .any(|item| item.workspace_id != self.workspace)
        {
            return Err(Error::profile_boundary(
                "daemon search response exceeds the configured workspace",
            ));
        }
        Ok(json!(response))
    }

    fn bind_scope(
        &self,
        profile: &mut Option<String>,
        workspace: &mut Option<String>,
    ) -> Result<()> {
        if profile.as_ref().is_some_and(|p| p != &self.profile)
            || workspace.as_ref().is_some_and(|w| w != &self.workspace)
        {
            return Err(Error::profile_boundary(
                "MCP arguments exceed the configured daemon profile/workspace",
            ));
        }
        *profile = Some(self.profile.clone());
        *workspace = Some(self.workspace.clone());
        Ok(())
    }

    fn request<T: DeserializeOwned + Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T> {
        let mut endpoint = self.endpoint.clone();
        endpoint.set_path(path);
        self.runtime.block_on(async {
            // The deadline wraps headers and the whole body, including slow reads.
            tokio::time::timeout(REQUEST_TIMEOUT, async {
                let mut request = self.client.request(method, endpoint);
                if let Some(body) = body {
                    request = request
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body);
                }
                let mut response = request.send().await.map_err(transport_error)?;
                let status = response.status();
                if status.is_redirection() {
                    return Err(Error::policy("daemon redirects are disabled"));
                }
                if status.as_u16() == 401 {
                    return Err(Error::auth_missing(
                        "daemon requires authentication; attachment cannot bypass daemon policy",
                    ));
                }
                if status.as_u16() == 403 {
                    return Err(Error::policy("daemon denied the requested capability"));
                }
                if response
                    .content_length()
                    .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
                {
                    return Err(Error::internal(
                        "daemon response exceeds the 2097152-byte limit",
                    ));
                }
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
                    if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                        return Err(Error::internal(
                            "daemon response exceeds the 2097152-byte limit",
                        ));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let envelope: Envelope<T> = serde_json::from_slice(&bytes)
                    .map_err(|_| Error::internal("daemon returned a malformed API response"))?;
                if envelope.provider.name != crate::PROVIDER_NAME {
                    return Err(Error::new(
                        ErrorCode::UnsupportedVersion,
                        "daemon returned an unsupported provider",
                    ));
                }
                match (
                    status.is_success(),
                    envelope.ok,
                    envelope.data,
                    envelope.error,
                ) {
                    (true, true, Some(data), None) => Ok(data),
                    (false, false, None, Some(error)) => Err(api_error(&error.code)),
                    _ => Err(Error::internal(
                        "daemon returned an inconsistent API response",
                    )),
                }
            })
            .await
            .unwrap_or_else(|_| Err(Error::storage("daemon request timed out")))
        })
    }
}

fn encode<T: Serialize>(request: &T) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec(request)
        .map_err(|_| Error::invalid_request("cannot encode daemon request"))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(Error::invalid_request(
            "daemon request exceeds the 1048576-byte limit",
        ));
    }
    Ok(bytes)
}
fn invalid_endpoint() -> Error {
    Error::invalid_request("daemon endpoint must be an HTTP loopback IP origin with an explicit port and without credentials, path, query, or fragment")
}
fn transport_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::storage("daemon request timed out")
    } else if error.is_connect() {
        Error::storage("existing daemon is unavailable")
    } else {
        Error::storage("daemon transport failed")
    }
}
fn api_error(code: &str) -> Error {
    let code = match code {
        "invalid_request" => ErrorCode::InvalidRequest,
        "missing_profile" => ErrorCode::MissingProfile,
        "missing_workspace" => ErrorCode::MissingWorkspace,
        "unknown_profile" => ErrorCode::UnknownProfile,
        "unknown_workspace" => ErrorCode::UnknownWorkspace,
        "storage_unavailable" => ErrorCode::StorageUnavailable,
        "policy_denied" => ErrorCode::PolicyDenied,
        "secret_detected" => ErrorCode::SecretDetected,
        "auth_missing" => ErrorCode::AuthMissing,
        "profile_boundary_denied" => ErrorCode::ProfileBoundaryDenied,
        "not_found" => ErrorCode::NotFound,
        "unsupported_version" => ErrorCode::UnsupportedVersion,
        "internal_error" => ErrorCode::InternalError,
        _ => return Error::internal("daemon returned an unrecognized API error"),
    };
    // Upstream messages can contain secrets, paths, or memory. Keep the code,
    // never copy its freeform message or warnings into the adapter error.
    Error::new(code, "daemon rejected the read request")
}
