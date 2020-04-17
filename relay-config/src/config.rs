use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use failure::{Backtrace, Context, Fail};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use relay_auth::{generate_key_pair, generate_relay_id, PublicKey, RelayId, SecretKey};
use relay_common::{Dsn, Uuid};
use relay_redis::RedisConfig;

use crate::types::ByteSize;
use crate::upstream::UpstreamDescriptor;

macro_rules! ctry {
    ($expr:expr, $kind:expr, $path:expr) => {
        match $expr {
            Ok(val) => val,
            Err(err) => return Err(ConfigError::for_file($path, err.context($kind))),
        }
    };
}

/// Defines the source of a config error
#[derive(Debug)]
enum ErrorSource {
    /// an error originating from a configuration file
    File(PathBuf),
    /// an error originating in a field override (an Env. Var. or a CLI parameter)
    FieldOverride(String),
}

/// Indicates config related errors.
#[derive(Debug)]
pub struct ConfigError {
    error_source: ErrorSource,
    inner: Context<ConfigErrorKind>,
}

impl ConfigError {
    fn for_file<P: AsRef<Path>>(p: P, inner: Context<ConfigErrorKind>) -> Self {
        ConfigError {
            error_source: ErrorSource::File(p.as_ref().to_path_buf()),
            inner,
        }
    }
    fn for_field<E>(name: &'static str, inner: E) -> Self
    where
        E: Fail,
    {
        ConfigError {
            error_source: ErrorSource::FieldOverride(name.into()),
            inner: inner.context(ConfigErrorKind::InvalidValue),
        }
    }

    /// Returns the error kind of the error.
    pub fn kind(&self) -> ConfigErrorKind {
        *self.inner.get_context()
    }
}

impl Fail for ConfigError {
    fn cause(&self) -> Option<&dyn Fail> {
        self.inner.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.inner.backtrace()
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.error_source {
            ErrorSource::File(file_name) => {
                write!(f, "{} (file {})", self.inner, file_name.display())
            }
            ErrorSource::FieldOverride(name) => write!(f, "{} (field {})", self.inner, name),
        }
    }
}

/// Indicates config related errors.
#[derive(Fail, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ConfigErrorKind {
    /// Failed to open the file.
    #[fail(display = "could not open config file")]
    CouldNotOpenFile,
    /// Failed to save a file.
    #[fail(display = "could not write config file")]
    CouldNotWriteFile,
    /// Parsing/dumping YAML failed.
    #[fail(display = "could not parse yaml config file")]
    BadYaml,
    /// Invalid config value
    #[fail(display = "invalid config value")]
    InvalidValue,
    /// The user attempted to run Relay with processing enabled, but uses a binary that was
    /// compiled without the processing feature.
    #[fail(display = "was not compiled with processing, cannot enable processing")]
    ProcessingNotAvailable,
}

/// Structure used to hold information about configuration overrides via
/// CLI parameters or environment variables
#[derive(Debug, Default)]
pub struct OverridableConfig {
    /// The upstream relay or sentry instance.
    pub upstream: Option<String>,
    /// The host the relay should bind to (network interface).
    pub host: Option<String>,
    /// The port to bind for the unencrypted relay HTTP server.
    pub port: Option<String>,
    /// "true" if processing is enabled "false" otherwise
    pub processing: Option<String>,
    /// the kafka bootstrap.servers configuration string
    pub kafka_url: Option<String>,
    /// the redis server url
    pub redis_url: Option<String>,
    /// The globally unique ID of the relay.
    pub id: Option<String>,
    /// The secret key of the relay
    pub secret_key: Option<String>,
    /// The public key of the relay
    pub public_key: Option<String>,
}

/// The relay credentials
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// The secret key of the relay
    pub secret_key: SecretKey,
    /// The public key of the relay
    pub public_key: PublicKey,
    /// The globally unique ID of the relay.
    pub id: RelayId,
}

impl ConfigObject for Credentials {
    fn format() -> ConfigFormat {
        ConfigFormat::Json
    }
    fn name() -> &'static str {
        "credentials"
    }
}

/// The operation mode of a relay.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RelayMode {
    /// This relay acts as a proxy for all requests and events.
    ///
    /// Events are normalized and rate limits from the upstream are enforced, but the relay will not
    /// fetch project configurations from the upstream or perform PII stripping. All events are
    /// accepted unless overridden on the file system.
    Proxy,

    /// This relay is configured statically in the file system.
    ///
    /// Events are only accepted for projects configured statically in the file system. All other
    /// events are rejected. If configured, PII stripping is also performed on those events.
    Static,

    /// Project configurations are managed by the upstream.
    ///
    /// Project configurations are always fetched from the upstream, unless they are statically
    /// overridden in the file system. This relay must be white-listed in the upstream Sentry. This
    /// is only possible, if the upstream is Sentry directly, or another managed Relay.
    Managed,

    /// Events are held in memory for inspection only.
    ///
    /// This mode is used for testing sentry SDKs.
    Capture,
}

impl fmt::Display for RelayMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RelayMode::Proxy => write!(f, "proxy"),
            RelayMode::Static => write!(f, "static"),
            RelayMode::Managed => write!(f, "managed"),
            RelayMode::Capture => write!(f, "capture"),
        }
    }
}

/// Checks if we are running in docker.
fn is_docker() -> bool {
    if fs::metadata("/.dockerenv").is_ok() {
        return true;
    }

    fs::read_to_string("/proc/self/cgroup")
        .map(|s| s.find("/docker").is_some())
        .unwrap_or(false)
}

/// Default value for the "bind" configuration.
fn default_host() -> IpAddr {
    if is_docker() {
        // Docker images rely on this service being exposed
        "0.0.0.0".parse().unwrap()
    } else {
        "127.0.0.1".parse().unwrap()
    }
}

/// Relay specific configuration values.
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct Relay {
    /// The operation mode of this relay.
    pub mode: RelayMode,
    /// The upstream relay or sentry instance.
    pub upstream: UpstreamDescriptor<'static>,
    /// The host the relay should bind to (network interface).
    pub host: IpAddr,
    /// The port to bind for the unencrypted relay HTTP server.
    pub port: u16,
    /// Optional port to bind for the encrypted relay HTTPS server.
    pub tls_port: Option<u16>,
    /// The path to the identity (DER-encoded PKCS12) to use for TLS.
    pub tls_identity_path: Option<PathBuf>,
    /// Password for the PKCS12 archive.
    pub tls_identity_password: Option<String>,
}

impl Default for Relay {
    fn default() -> Self {
        Relay {
            mode: RelayMode::Managed,
            upstream: "https://sentry.io/".parse().unwrap(),
            host: default_host(),
            port: 3000,
            tls_port: None,
            tls_identity_path: None,
            tls_identity_password: None,
        }
    }
}

/// Controls the log format
#[derive(Serialize, Deserialize, Debug, Copy, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Auto detect (pretty for tty, simplified for other)
    Auto,
    /// With colors
    Pretty,
    /// Simplified log output
    Simplified,
    /// Dump out JSON lines
    Json,
}

/// Controls the logging system.
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct Logging {
    /// The log level for the relay.
    level: log::LevelFilter,
    /// If set to true this emits log messages for failed event payloads.
    log_failed_payloads: bool,
    /// Controls the log format.
    format: LogFormat,
    /// When set to true, backtraces are forced on.
    enable_backtraces: bool,
}

impl Default for Logging {
    fn default() -> Self {
        Logging {
            level: log::LevelFilter::Info,
            log_failed_payloads: false,
            format: LogFormat::Auto,
            enable_backtraces: false,
        }
    }
}

/// Control the metrics.
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct Metrics {
    /// If set to a host/port string then metrics will be reported to this
    /// statsd instance.
    statsd: Option<String>,
    /// The prefix that should be added to all metrics.
    prefix: String,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            statsd: None,
            prefix: "sentry.relay".into(),
        }
    }
}

/// Controls various limits
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct Limits {
    /// How many requests can be sent concurrently from Relay to the upstream before Relay starts
    /// buffering.
    max_concurrent_requests: usize,
    /// How many queries can be sent concurrently from Relay to the upstream before Relay starts
    /// buffering.
    ///
    /// The concurrency of queries is additionally constrained by `max_concurrent_requests`.
    max_concurrent_queries: usize,
    /// The maximum payload size for events.
    max_event_size: ByteSize,
    /// The maximum size for each attachment.
    max_attachment_size: ByteSize,
    /// The maximum combined size for all attachments in an envelope or request.
    max_attachments_size: ByteSize,
    /// The maximum payload size for an entire envelopes. Individual limits still apply.
    max_envelope_size: ByteSize,
    /// The maximum number of session items per envelope.
    max_session_count: usize,
    /// The maximum payload size for general API requests.
    max_api_payload_size: ByteSize,
    /// The maximum payload size for file uploads and chunks.
    max_api_file_upload_size: ByteSize,
    /// The maximum payload size for chunks
    max_api_chunk_upload_size: ByteSize,
    /// The maximum number of threads to spawn for CPU and web work, each.
    ///
    /// The total number of threads spawned will roughly be `2 * max_thread_count + 1`. Defaults to
    /// the number of logical CPU cores on the host.
    max_thread_count: usize,
    /// The maximum number of seconds a query is allowed to take across retries. Individual requests
    /// have lower timeouts. Defaults to 30 seconds.
    query_timeout: u64,
    /// The maximum number of connections to Relay that can be created at once.
    max_connection_rate: usize,
    /// The maximum number of pending connects to Relay. This corresponds to the backlog param of
    /// `listen(2)` in POSIX.
    max_pending_connections: i32,
    /// The maximum number of open connections to Relay.
    max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_concurrent_requests: 100,
            max_concurrent_queries: 5,
            max_event_size: ByteSize::from_megabytes(1),
            max_attachment_size: ByteSize::from_megabytes(50),
            max_attachments_size: ByteSize::from_megabytes(50),
            max_envelope_size: ByteSize::from_megabytes(50),
            max_session_count: 100,
            max_api_payload_size: ByteSize::from_megabytes(20),
            max_api_file_upload_size: ByteSize::from_megabytes(40),
            max_api_chunk_upload_size: ByteSize::from_megabytes(100),
            max_thread_count: num_cpus::get(),
            query_timeout: 30,
            max_connection_rate: 256,
            max_pending_connections: 2048,
            max_connections: 25_000,
        }
    }
}

/// Controls authentication with upstream.
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct Http {
    /// Timeout for upstream requests in seconds.
    timeout: u32,
    /// Maximum interval between failed request retries in seconds.
    max_retry_interval: u32,
    /// The custom HTTP Host header to send to the upstream.
    host_header: Option<String>,
}

impl Default for Http {
    fn default() -> Self {
        Http {
            timeout: 5,
            max_retry_interval: 60,
            host_header: None,
        }
    }
}

/// Controls internal caching behavior.
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct Cache {
    /// The cache timeout for project configurations in seconds.
    project_expiry: u32,
    /// Continue using project state this many seconds after cache expiry while a new state is
    /// being fetched. This is added on top of `project_expiry` and `miss_expiry`. Default is 0.
    project_grace_period: u32,
    /// The cache timeout for downstream relay info (public keys) in seconds.
    relay_expiry: u32,
    /// The cache timeout for events (store) before dropping them.
    event_expiry: u32,
    /// The maximum amount of events to queue before dropping them.
    event_buffer_size: u32,
    /// The cache timeout for non-existing entries.
    miss_expiry: u32,
    /// The buffer timeout for batched queries before sending them upstream in ms.
    batch_interval: u32,
    /// The maximum number of project configs to fetch from Sentry at once. Defaults to 500.
    ///
    /// `cache.batch_interval` controls how quickly batches are sent, this controls the batch size.
    batch_size: usize,
    /// Interval for watching local cache override files in seconds.
    file_interval: u32,
    /// Interval for evicting outdated project configs from memory.
    eviction_interval: u32,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            project_expiry: 300, // 5 minutes
            project_grace_period: 0,
            relay_expiry: 3600, // 1 hour
            event_expiry: 600,  // 10 minutes
            event_buffer_size: 1000,
            miss_expiry: 60,     // 1 minute
            batch_interval: 100, // 100ms
            batch_size: 500,
            file_interval: 10,     // 10 seconds
            eviction_interval: 60, // 60 seconds
        }
    }
}

/// Controls interal reporting to Sentry.
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
struct Sentry {
    dsn: Option<Dsn>,
    enabled: bool,
}

impl Default for Sentry {
    fn default() -> Self {
        Sentry {
            dsn: "https://0cc4a37e5aab4da58366266a87a95740@sentry.io/1269704"
                .parse()
                .ok(),
            enabled: false,
        }
    }
}

/// Define the topics over which Relay communicates with Sentry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KafkaTopic {
    /// Simple events (without attachments) topic.
    Events,
    /// Complex events (with attachments) topic.
    Attachments,
    /// Transaction events topic.
    Transactions,
    /// Shared outcomes topic for Relay and Sentry.
    Outcomes,
    /// Session health updates.
    Sessions,
}

/// Configuration for topics.
#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
pub struct TopicNames {
    /// Simple events topic name.
    pub events: String,
    /// Events with attachments topic name.
    pub attachments: String,
    /// Transaction events topic name.
    pub transactions: String,
    /// Event outcomes topic name.
    pub outcomes: String,
    /// Session health topic name.
    pub sessions: String,
}

impl Default for TopicNames {
    fn default() -> Self {
        Self {
            events: "ingest-events".to_owned(),
            attachments: "ingest-attachments".to_owned(),
            transactions: "ingest-transactions".to_owned(),
            outcomes: "outcomes".to_owned(),
            sessions: "ingest-sessions".to_owned(),
        }
    }
}

/// A name value pair of Kafka config parameter.
#[derive(Serialize, Deserialize, Debug)]
pub struct KafkaConfigParam {
    /// Name of the Kafka config parameter.
    pub name: String,
    /// Value of the Kafka config parameter.
    pub value: String,
}

fn default_max_secs_in_future() -> u32 {
    60 // 1 minute
}

fn default_max_secs_in_past() -> u32 {
    30 * 24 * 3600 // 30 days
}

fn default_chunk_size() -> ByteSize {
    ByteSize::from_megabytes(1)
}

fn default_projectconfig_cache_prefix() -> String {
    "relayconfig".to_owned()
}

fn default_max_rate_limit() -> Option<u32> {
    Some(300) // 5 minutes
}

/// Controls Sentry-internal event processing.
#[derive(Serialize, Deserialize, Debug)]
pub struct Processing {
    /// True if the Relay should do processing. Defaults to `false`.
    pub enabled: bool,
    /// GeoIp DB file location.
    #[serde(default)]
    pub geoip_path: Option<PathBuf>,
    /// Maximum future timestamp of ingested events.
    #[serde(default = "default_max_secs_in_future")]
    pub max_secs_in_future: u32,
    /// Maximum age of ingested events. Older events will be adjusted to `now()`.
    #[serde(default = "default_max_secs_in_past")]
    pub max_secs_in_past: u32,
    /// Kafka producer configurations.
    pub kafka_config: Vec<KafkaConfigParam>,
    /// Kafka topic names.
    #[serde(default)]
    pub topics: TopicNames,
    /// Redis hosts to connect to for storing state for rate limits.
    #[serde(default)]
    pub redis: Option<RedisConfig>,
    /// Maximum chunk size of attachments for Kafka.
    #[serde(default = "default_chunk_size")]
    pub attachment_chunk_size: ByteSize,
    /// Prefix to use when looking up project configs in Redis. Defaults to "relayconfig".
    #[serde(default = "default_projectconfig_cache_prefix")]
    pub projectconfig_cache_prefix: String,
    /// Maximum rate limit to report to clients.
    #[serde(default = "default_max_rate_limit")]
    pub max_rate_limit: Option<u32>,
}

impl Default for Processing {
    /// Constructs a disabled processing configuration.
    fn default() -> Self {
        Self {
            enabled: false,
            geoip_path: None,
            max_secs_in_future: 0,
            max_secs_in_past: 0,
            kafka_config: Vec::new(),
            topics: TopicNames::default(),
            redis: None,
            attachment_chunk_size: default_chunk_size(),
            projectconfig_cache_prefix: default_projectconfig_cache_prefix(),
            max_rate_limit: default_max_rate_limit(),
        }
    }
}

/// Minimal version of a config for dumping out.
#[derive(Serialize, Deserialize, Debug, Default)]
pub struct MinimalConfig {
    /// The relay part of the config.
    pub relay: Relay,
}

impl MinimalConfig {
    /// Saves the config in the given config folder as config.yml
    pub fn save_in_folder<P: AsRef<Path>>(&self, p: P) -> Result<(), ConfigError> {
        if fs::metadata(p.as_ref()).is_err() {
            ctry!(
                fs::create_dir_all(p.as_ref()),
                ConfigErrorKind::CouldNotOpenFile,
                p.as_ref()
            );
        }
        self.save(p.as_ref())
    }
}

impl ConfigObject for MinimalConfig {
    fn format() -> ConfigFormat {
        ConfigFormat::Yaml
    }

    fn name() -> &'static str {
        "config"
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct ConfigValues {
    #[serde(default)]
    relay: Relay,
    #[serde(default)]
    http: Http,
    #[serde(default)]
    cache: Cache,
    #[serde(default)]
    limits: Limits,
    #[serde(default)]
    logging: Logging,
    #[serde(default)]
    metrics: Metrics,
    #[serde(default)]
    sentry: Sentry,
    #[serde(default)]
    processing: Processing,
}

impl ConfigObject for ConfigValues {
    fn format() -> ConfigFormat {
        ConfigFormat::Yaml
    }
    fn name() -> &'static str {
        "config"
    }
}

/// Config struct.
pub struct Config {
    values: ConfigValues,
    credentials: Option<Credentials>,
    path: PathBuf,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("path", &self.path)
            .field("values", &self.values)
            .finish()
    }
}

impl Config {
    /// Loads a config from a given config folder.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Config, ConfigError> {
        let path = env::current_dir()
            .map(|x| x.join(path.as_ref()))
            .unwrap_or_else(|_| path.as_ref().to_path_buf());
        let config = Config {
            values: ConfigValues::load(&path)?,
            credentials: if fs::metadata(Credentials::path(&path)).is_ok() {
                Some(Credentials::load(&path)?)
            } else {
                None
            },
            path: path.clone(),
        };

        if cfg!(not(feature = "processing")) && config.processing_enabled() {
            return Err(ConfigError::for_file(
                &path,
                ConfigErrorKind::ProcessingNotAvailable.into(),
            ));
        }

        Ok(config)
    }

    /// Override configuration with values coming from other sources (e.g. env variables or
    /// command line parameters)
    pub fn apply_override(
        &mut self,
        overrides: OverridableConfig,
    ) -> Result<&mut Self, ConfigError> {
        let relay = &mut self.values.relay;

        if let Some(upstream) = overrides.upstream {
            relay.upstream = upstream
                .parse()
                .map_err(|err| ConfigError::for_field("upstream", err))?;
        }

        if let Some(host) = overrides.host {
            relay.host = host
                .parse()
                .map_err(|err| ConfigError::for_field("host", err))?;
        }

        if let Some(port) = overrides.port {
            relay.port = u16::from_str_radix(port.as_str(), 10)
                .map_err(|err| ConfigError::for_field("port", err))?;
        }

        let processing = &mut self.values.processing;
        if let Some(enabled) = overrides.processing {
            match enabled.to_lowercase().as_str() {
                "true" | "1" => processing.enabled = true,
                "false" | "0" | "" => processing.enabled = false,
                _ => {
                    return Err(ConfigError::for_field(
                        "processing",
                        ConfigErrorKind::InvalidValue,
                    ))
                }
            }
        }

        if let Some(redis) = overrides.redis_url {
            processing.redis = Some(RedisConfig::Single(redis))
        }

        if let Some(kafka_url) = overrides.kafka_url {
            let existing = processing
                .kafka_config
                .iter_mut()
                .find(|e| e.name == "bootstrap.servers");

            if let Some(config_param) = existing {
                config_param.value = kafka_url;
            } else {
                processing.kafka_config.push(KafkaConfigParam {
                    name: "bootstrap.servers".to_owned(),
                    value: kafka_url,
                })
            }
        }
        // credentials overrides
        let id = if let Some(id) = overrides.id {
            let id = Uuid::parse_str(&id).map_err(|err| ConfigError::for_field("id", err))?;
            Some(id)
        } else {
            None
        };
        let public_key = if let Some(public_key) = overrides.public_key {
            let public_key = public_key
                .parse()
                .map_err(|err| ConfigError::for_field("public_key", err))?;
            Some(public_key)
        } else {
            None
        };

        let secret_key = if let Some(secret_key) = overrides.secret_key {
            let secret_key = secret_key
                .parse()
                .map_err(|err| ConfigError::for_field("secret_key", err))?;
            Some(secret_key)
        } else {
            None
        };

        if let Some(credentials) = &mut self.credentials {
            //we have existing credentials we may override some entries
            if let Some(id) = id {
                credentials.id = id;
            }
            if let Some(public_key) = public_key {
                credentials.public_key = public_key;
            }
            if let Some(secret_key) = secret_key {
                credentials.secret_key = secret_key
            }
        } else {
            //no existing credentials we may only create the full credentials
            match (id, public_key, secret_key) {
                (Some(id), Some(public_key), Some(secret_key)) => {
                    self.credentials = Some(Credentials {
                        id,
                        public_key,
                        secret_key,
                    })
                }
                (None, None, None) => {
                    // nothing provided, we'll just leave the credentials None, maybe we
                    // don't need them in the current command or we'll override them later
                }
                _ => {
                    return Err(ConfigError::for_field(
                        "incomplete credentials",
                        ConfigErrorKind::InvalidValue,
                    ));
                }
            }
        }

        Ok(self)
    }

    /// Checks if the config is already initialized.
    pub fn config_exists<P: AsRef<Path>>(path: P) -> bool {
        fs::metadata(ConfigValues::path(path.as_ref())).is_ok()
    }

    /// Returns the filename of the config file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Dumps out a YAML string of the values.
    pub fn to_yaml_string(&self) -> Result<String, ConfigError> {
        Ok(ctry!(
            serde_yaml::to_string(&self.values),
            ConfigErrorKind::InvalidValue,
            &self.path
        ))
    }

    /// Regenerates the relay credentials.
    ///
    /// This also writes the credentials back to the file.
    pub fn regenerate_credentials(&mut self) -> Result<(), ConfigError> {
        log::info!("generating new relay credentials");
        let (sk, pk) = generate_key_pair();
        let creds = Credentials {
            secret_key: sk,
            public_key: pk,
            id: generate_relay_id(),
        };
        creds.save(&self.path)?;
        self.credentials = Some(creds);
        Ok(())
    }

    /// Return the current credentials
    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    /// Set new credentials.
    pub fn replace_credentials(
        &mut self,
        credentials: Option<Credentials>,
    ) -> Result<bool, ConfigError> {
        if self.credentials == credentials {
            return Ok(false);
        }
        match credentials {
            Some(creds) => {
                creds.save(&self.path)?;
                self.credentials = Some(creds);
            }
            None => {
                let path = Credentials::path(&self.path);
                if fs::metadata(&path).is_ok() {
                    ctry!(
                        fs::remove_file(&path),
                        ConfigErrorKind::CouldNotWriteFile,
                        &path
                    );
                }
            }
        }
        Ok(true)
    }

    /// Returns `true` if the config is ready to use.
    pub fn has_credentials(&self) -> bool {
        self.credentials.is_some()
    }

    /// Returns the secret key if set.
    pub fn secret_key(&self) -> Option<&SecretKey> {
        self.credentials.as_ref().map(|x| &x.secret_key)
    }

    /// Returns the public key if set.
    pub fn public_key(&self) -> Option<&PublicKey> {
        self.credentials.as_ref().map(|x| &x.public_key)
    }

    /// Returns the relay ID.
    pub fn relay_id(&self) -> Option<&RelayId> {
        self.credentials.as_ref().map(|x| &x.id)
    }

    /// Returns the relay mode.
    pub fn relay_mode(&self) -> RelayMode {
        self.values.relay.mode
    }

    /// Returns the upstream target as descriptor.
    pub fn upstream_descriptor(&self) -> &UpstreamDescriptor<'_> {
        &self.values.relay.upstream
    }

    /// Returns the custom HTTP "Host" header.
    pub fn http_host_header(&self) -> Option<&str> {
        self.values.http.host_header.as_deref()
    }

    /// Returns the listen address.
    pub fn listen_addr(&self) -> SocketAddr {
        (self.values.relay.host, self.values.relay.port).into()
    }

    /// Returns the TLS listen address.
    pub fn tls_listen_addr(&self) -> Option<SocketAddr> {
        if self.values.relay.tls_identity_path.is_some() {
            let port = self.values.relay.tls_port.unwrap_or(3443);
            Some((self.values.relay.host, port).into())
        } else {
            None
        }
    }

    /// Returns the path to the identity bundle
    pub fn tls_identity_path(&self) -> Option<&Path> {
        self.values.relay.tls_identity_path.as_deref()
    }

    /// Returns the password for the identity bundle
    pub fn tls_identity_password(&self) -> Option<&str> {
        self.values.relay.tls_identity_password.as_deref()
    }

    /// Returns the log level.
    pub fn log_level_filter(&self) -> log::LevelFilter {
        self.values.logging.level
    }

    /// Should backtraces be enabled?
    pub fn enable_backtraces(&self) -> bool {
        self.values.logging.enable_backtraces
    }

    /// Should we debug log bad payloads?
    pub fn log_failed_payloads(&self) -> bool {
        self.values.logging.log_failed_payloads
    }

    /// Which log format should be used?
    pub fn log_format(&self) -> LogFormat {
        self.values.logging.format
    }

    /// Returns the socket addresses for statsd.
    ///
    /// If stats is disabled an empty vector is returned.
    pub fn statsd_addrs(&self) -> Result<Vec<SocketAddr>, ConfigError> {
        if let Some(ref addr) = self.values.metrics.statsd {
            Ok(ctry!(
                addr.as_str().to_socket_addrs(),
                ConfigErrorKind::InvalidValue,
                &self.path
            )
            .collect())
        } else {
            Ok(vec![])
        }
    }

    /// Return the prefix for statsd metrics.
    pub fn metrics_prefix(&self) -> &str {
        &self.values.metrics.prefix
    }

    /// Returns the default timeout for all upstream HTTP requests.
    pub fn http_timeout(&self) -> Duration {
        Duration::from_secs(self.values.http.timeout.into())
    }

    /// Returns the failed upstream request retry interval.
    pub fn http_max_retry_interval(&self) -> Duration {
        Duration::from_secs(self.values.http.max_retry_interval.into())
    }

    /// Returns the expiry timeout for cached projects.
    pub fn project_cache_expiry(&self) -> Duration {
        Duration::from_secs(self.values.cache.project_expiry.into())
    }

    /// Returns the expiry timeout for cached relay infos (public keys).
    pub fn relay_cache_expiry(&self) -> Duration {
        Duration::from_secs(self.values.cache.relay_expiry.into())
    }

    /// Returns the timeout for buffered events (due to upstream errors).
    pub fn event_buffer_expiry(&self) -> Duration {
        Duration::from_secs(self.values.cache.event_expiry.into())
    }

    /// Returns the maximum number of buffered events
    pub fn event_buffer_size(&self) -> u32 {
        self.values.cache.event_buffer_size
    }

    /// Returns the expiry timeout for cached misses before trying to refetch.
    pub fn cache_miss_expiry(&self) -> Duration {
        Duration::from_secs(self.values.cache.miss_expiry.into())
    }

    /// Returns the grace period for project caches.
    pub fn project_grace_period(&self) -> Duration {
        Duration::from_secs(self.values.cache.project_grace_period.into())
    }

    /// Returns the number of seconds during which batchable queries are collected before sending
    /// them in a single request.
    pub fn query_batch_interval(&self) -> Duration {
        Duration::from_millis(self.values.cache.batch_interval.into())
    }

    /// Returns the interval in seconds in which local project configurations should be reloaded.
    pub fn local_cache_interval(&self) -> Duration {
        Duration::from_secs(self.values.cache.file_interval.into())
    }

    /// Returns the interval in seconds in which projects configurations should be freed from
    /// memory when expired.
    pub fn cache_eviction_interval(&self) -> Duration {
        Duration::from_secs(self.values.cache.eviction_interval.into())
    }

    /// Returns the maximum size of an event payload in bytes.
    pub fn max_event_size(&self) -> usize {
        self.values.limits.max_event_size.as_bytes() as usize
    }

    /// Returns the maximum size of each attachment.
    pub fn max_attachment_size(&self) -> usize {
        self.values.limits.max_attachment_size.as_bytes() as usize
    }

    /// Returns the maxmium combined size of attachments or payloads containing attachments
    /// (minidump, unreal, standalone attachments) in bytes.
    pub fn max_attachments_size(&self) -> usize {
        self.values.limits.max_attachments_size.as_bytes() as usize
    }

    /// Returns the maximum size of an envelope payload in bytes.
    ///
    /// Individual item size limits still apply.
    pub fn max_envelope_size(&self) -> usize {
        self.values.limits.max_envelope_size.as_bytes() as usize
    }

    /// Returns the maximum number of sessions per envelope.
    pub fn max_session_count(&self) -> usize {
        self.values.limits.max_session_count
    }

    /// Returns the maximum payload size for general API requests.
    pub fn max_api_payload_size(&self) -> usize {
        self.values.limits.max_api_payload_size.as_bytes() as usize
    }

    /// Returns the maximum payload size for file uploads and chunks.
    pub fn max_api_file_upload_size(&self) -> usize {
        self.values.limits.max_api_file_upload_size.as_bytes() as usize
    }

    /// Returns the maximum payload size for chunks
    pub fn max_api_chunk_upload_size(&self) -> usize {
        self.values.limits.max_api_chunk_upload_size.as_bytes() as usize
    }

    /// Returns the maximum number of active requests
    pub fn max_concurrent_requests(&self) -> usize {
        self.values.limits.max_concurrent_requests
    }

    /// Returns the maximum number of active queries
    pub fn max_concurrent_queries(&self) -> usize {
        self.values.limits.max_concurrent_queries
    }

    /// The maximum number of seconds a query is allowed to take across retries.
    pub fn query_timeout(&self) -> Duration {
        Duration::from_secs(self.values.limits.query_timeout)
    }

    /// The maximum number of open connections to Relay.
    pub fn max_connections(&self) -> usize {
        self.values.limits.max_connections
    }

    /// The maximum number of connections to Relay that can be created at once.
    pub fn max_connection_rate(&self) -> usize {
        self.values.limits.max_connection_rate
    }

    /// The maximum number of pending connects to Relay.
    pub fn max_pending_connections(&self) -> i32 {
        self.values.limits.max_pending_connections
    }

    /// Returns the number of cores to use for thread pools.
    pub fn cpu_concurrency(&self) -> usize {
        self.values.limits.max_thread_count
    }

    /// Returns the maximum size of a project config query.
    pub fn query_batch_size(&self) -> usize {
        self.values.cache.batch_size
    }

    /// Return the Sentry DSN if reporting to Sentry is enabled.
    pub fn sentry_dsn(&self) -> Option<&Dsn> {
        if self.values.sentry.enabled {
            self.values.sentry.dsn.as_ref()
        } else {
            None
        }
    }

    /// Get filename for static project config.
    pub fn project_configs_path(&self) -> PathBuf {
        self.path.join("projects")
    }

    /// True if the Relay should do processing.
    pub fn processing_enabled(&self) -> bool {
        self.values.processing.enabled
    }

    /// The path to the GeoIp database required for event processing.
    pub fn geoip_path(&self) -> Option<&Path> {
        self.values.processing.geoip_path.as_deref()
    }

    /// Maximum future timestamp of ingested events.
    pub fn max_secs_in_future(&self) -> i64 {
        self.values.processing.max_secs_in_future.into()
    }

    /// Maximum age of ingested events. Older events will be adjusted to `now()`.
    pub fn max_secs_in_past(&self) -> i64 {
        self.values.processing.max_secs_in_past.into()
    }

    /// The list of Kafka configuration parameters.
    pub fn kafka_config(&self) -> &[KafkaConfigParam] {
        self.values.processing.kafka_config.as_slice()
    }

    /// Returns the name of the specified Kafka topic.
    pub fn kafka_topic_name(&self, topic: KafkaTopic) -> &str {
        let topics = &self.values.processing.topics;
        match topic {
            KafkaTopic::Attachments => topics.attachments.as_str(),
            KafkaTopic::Events => topics.events.as_str(),
            KafkaTopic::Transactions => topics.transactions.as_str(),
            KafkaTopic::Outcomes => topics.outcomes.as_str(),
            KafkaTopic::Sessions => topics.sessions.as_str(),
        }
    }

    /// Redis servers to connect to, for rate limiting.
    pub fn redis(&self) -> Option<&RedisConfig> {
        self.values.processing.redis.as_ref()
    }

    /// Chunk size of attachments in bytes.
    pub fn attachment_chunk_size(&self) -> usize {
        self.values.processing.attachment_chunk_size.as_bytes() as usize
    }

    /// Default prefix to use when looking up project configs in Redis. This is only done when
    /// Relay is in processing mode.
    pub fn projectconfig_cache_prefix(&self) -> &str {
        &self.values.processing.projectconfig_cache_prefix
    }

    /// Maximum rate limit to report to clients in seconds.
    pub fn max_rate_limit(&self) -> Option<u64> {
        self.values.processing.max_rate_limit.map(u32::into)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            values: ConfigValues::default(),
            credentials: None,
            path: PathBuf::new(),
        }
    }
}

enum ConfigFormat {
    Yaml,
    Json,
}

impl ConfigFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            ConfigFormat::Yaml => "yml",
            ConfigFormat::Json => "json",
        }
    }
}

trait ConfigObject: DeserializeOwned + Serialize {
    fn format() -> ConfigFormat;
    fn name() -> &'static str;
    fn path(base: &Path) -> PathBuf {
        base.join(format!("{}.{}", Self::name(), Self::format().extension()))
    }

    fn load(base: &Path) -> Result<Self, ConfigError> {
        let path = Self::path(base);
        let f = ctry!(
            fs::File::open(&path),
            ConfigErrorKind::CouldNotOpenFile,
            &path
        );
        Ok(match Self::format() {
            ConfigFormat::Yaml => ctry!(
                serde_yaml::from_reader(io::BufReader::new(f)),
                ConfigErrorKind::BadYaml,
                &path
            ),
            ConfigFormat::Json => ctry!(
                serde_json::from_reader(io::BufReader::new(f)),
                ConfigErrorKind::BadYaml,
                &path
            ),
        })
    }

    fn save(&self, base: &Path) -> Result<(), ConfigError> {
        let path = Self::path(base);
        let mut options = fs::OpenOptions::new();
        options.write(true).truncate(true).create(true);

        // Remove all non-user permissions for the newly created file
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut f = ctry!(
            options.open(&path),
            ConfigErrorKind::CouldNotWriteFile,
            &path
        );

        match Self::format() {
            ConfigFormat::Yaml => {
                ctry!(
                    serde_yaml::to_writer(&mut f, self),
                    ConfigErrorKind::BadYaml,
                    &path
                );
            }
            ConfigFormat::Json => {
                ctry!(
                    serde_json::to_writer_pretty(&mut f, self),
                    ConfigErrorKind::BadYaml,
                    &path
                );
            }
        }
        f.write_all(b"\n").ok();
        Ok(())
    }
}
