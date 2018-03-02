use chrono::Duration;
use hyper::{Method, Request, Uri};
use hyper::header::{ContentLength, ContentType};
use serde::Serialize;

use auth::{PublicKey, RelayId, SecretKey};
use upstream::UpstreamDescriptor;

/// Holds common config values that affect the aorta behavior.
///
/// This config is typically created by something and then passed down
/// through the aorta functionality to affect how they behave.  This is
/// in turn also used by the trove crate to manage the individual aortas.
#[derive(Debug)]
pub struct AortaConfig {
    /// How long it takes until a snapshot is considered expired.
    pub snapshot_expiry: Duration,
    /// How many seconds between failed auth attempts.
    pub auth_retry_interval: Duration,
    /// How many seconds between heartbeats
    pub heartbeat_interval: Duration,
    /// The upstream descriptor for this aorta
    pub upstream: UpstreamDescriptor<'static>,
    /// The relay ID.
    pub relay_id: Option<RelayId>,
    /// The private key for authentication.
    pub secret_key: Option<SecretKey>,
    /// The public key for authentication.
    pub public_key: Option<PublicKey>,
}

impl Default for AortaConfig {
    fn default() -> AortaConfig {
        AortaConfig {
            snapshot_expiry: Duration::seconds(60),
            auth_retry_interval: Duration::seconds(15),
            heartbeat_interval: Duration::seconds(30),
            upstream: Default::default(),
            relay_id: None,
            secret_key: None,
            public_key: None,
        }
    }
}

impl AortaConfig {
    /// Returns the relay id or panics.
    pub fn relay_id(&self) -> &RelayId {
        self.relay_id
            .as_ref()
            .expect("relay id must be set on aorta config")
    }

    /// Returns the public key or panics.
    pub fn public_key(&self) -> &PublicKey {
        self.public_key
            .as_ref()
            .expect("public key must be set on aorta config")
    }

    /// Returns the secret key or panics.
    pub fn secret_key(&self) -> &SecretKey {
        self.secret_key
            .as_ref()
            .expect("secret key must be set on aorta config")
    }

    /// Returns the URL to hit for an api request on the upstream.
    pub fn get_api_uri(&self, path: &str) -> Uri {
        format!(
            "{}api/0/{}",
            self.upstream,
            path.trim_left_matches(&['/'][..])
        ).parse()
            .unwrap()
    }

    /// Prepares a JSON bodied API request to aorta with signature.
    pub fn prepare_aorta_req<S: Serialize>(&self, method: Method, path: &str, body: &S) -> Request {
        let mut req = Request::new(method, self.get_api_uri(path));
        let (json, signature) = self.secret_key().pack(body);
        {
            let headers = req.headers_mut();
            headers.set_raw("X-Sentry-Relay-Id", self.relay_id().to_string());
            headers.set_raw("X-Sentry-Relay-Signature", signature);
            headers.set(ContentType::json());
            headers.set(ContentLength(json.len() as u64));
        }
        req.set_body(json);
        req
    }
}
