//! The S3 client and the interceptors that make it speak to the RunPod gateway.
//!
//! The gateway serves an NFS volume behind an S3-shaped API, and differs from a
//! real S3 service in six places. Each one is fixed by an interceptor below,
//! and every fix runs *before* the request is signed: SigV4 covers the URI and
//! the headers, so a change made afterwards would invalidate the signature.

use std::error::Error;
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::config::timeout::TimeoutConfig;
use aws_sdk_s3::config::interceptors::{
    BeforeDeserializationInterceptorContextMut, BeforeTransmitInterceptorContextMut,
};
use aws_sdk_s3::config::{
    BehaviorVersion, ConfigBag, Credentials, Intercept, Region, RequestChecksumCalculation,
    ResponseChecksumValidation, RuntimeComponents,
};
use aws_sdk_s3::error::{BoxError, DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::ByteStream;

pub type BoxErr = Box<dyn Error>;

/// Removes the slash the SDK leaves after the bucket name (`/{bucket}/?…`),
/// which the gateway rejects as an invalid object path.
#[derive(Debug)]
pub struct StripBucketTrailingSlash {
    pub bucket: String,
}

impl Intercept for StripBucketTrailingSlash {
    fn name(&self) -> &'static str {
        "StripBucketTrailingSlash"
    }

    fn modify_before_signing(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _rc: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let request = context.request_mut();
        let uri = request.uri().to_string();

        let (base, query) = match uri.split_once('?') {
            Some((b, q)) => (b, Some(q)),
            None => (uri.as_str(), None),
        };

        let suffix = format!("/{}/", self.bucket);
        if !base.ends_with(&suffix) {
            return Ok(());
        }

        let fixed_base = &base[..base.len() - 1];
        let new_uri = match query {
            Some(q) => format!("{fixed_base}?{q}"),
            None => fixed_base.to_string(),
        };

        request.set_uri(new_uri)?;
        Ok(())
    }
}

/// Rewrites the non-standard `Last-Modified: … UTC` the gateway sends into the
/// `… GMT` the SDK's date parser accepts.
///
/// Without it a perfectly good 200 response fails to parse.
#[derive(Debug)]
pub struct FixLastModifiedUtc;

impl Intercept for FixLastModifiedUtc {
    fn name(&self) -> &'static str {
        "FixLastModifiedUtc"
    }

    fn modify_before_deserialization(
        &self,
        context: &mut BeforeDeserializationInterceptorContextMut<'_>,
        _rc: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let response = context.response_mut();
        let fixed = match response.headers().get("last-modified") {
            Some(v) if v.ends_with(" UTC") => v.replace(" UTC", " GMT"),
            _ => return Ok(()),
        };
        response.headers_mut().try_insert("last-modified", fixed)?;
        Ok(())
    }
}

/// Drops `content-type` and `content-length` from writes before signing.
///
/// The gateway was built against a client that never signs those two, so having
/// them in the signature makes its own string-to-sign differ and the request is
/// refused.
#[derive(Debug)]
pub struct DropWriteEntityHeaders;

impl Intercept for DropWriteEntityHeaders {
    fn name(&self) -> &'static str {
        "DropWriteEntityHeaders"
    }

    fn modify_before_signing(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _rc: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let headers = context.request_mut().headers_mut();
        // Only the SDK's default binary content type goes; an XML body keeps
        // its own, because the gateway has to parse it.
        if headers
            .get("content-type")
            .is_some_and(|v| v == "application/octet-stream")
        {
            headers.remove("content-type");
        }
        headers.remove("content-length");
        Ok(())
    }
}

/// Removes the SDK's `?x-id=<Operation>` query parameter before signing.
///
/// The gateway does not know the parameter and drops it before verifying, so a
/// signature that covers it never matches. Writes are what break: reads carry
/// real parameters alongside it.
#[derive(Debug)]
pub struct StripXIdParam;

impl Intercept for StripXIdParam {
    fn name(&self) -> &'static str {
        "StripXIdParam"
    }

    fn modify_before_signing(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _rc: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let request = context.request_mut();
        let uri = request.uri().to_string();
        let Some((base, query)) = uri.split_once('?') else {
            return Ok(());
        };
        let kept: Vec<&str> = query
            .split('&')
            .filter(|kv| *kv != "x-id" && !kv.starts_with("x-id="))
            .collect();
        let new_uri = if kept.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", kept.join("&"))
        };
        if new_uri != uri {
            request.set_uri(new_uri)?;
        }
        Ok(())
    }
}

/// Signs every request with `UNSIGNED-PAYLOAD` instead of a real body hash.
///
/// A plain `PutObject` signed over its payload is rejected by the gateway;
/// listing, reading and multipart parts already use unsigned or streaming
/// signatures, which is why only simple writes broke.
#[derive(Debug)]
pub struct ForceUnsignedPayload;

impl Intercept for ForceUnsignedPayload {
    fn name(&self) -> &'static str {
        "ForceUnsignedPayload"
    }

    // Set before the retry loop so every attempt inherits it.
    fn modify_before_retry_loop(
        &self,
        _context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _rc: &RuntimeComponents,
        cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        cfg.interceptor_state()
            .store_put(aws_runtime::auth::PayloadSigningOverride::UnsignedPayload);
        Ok(())
    }

    // Set again right before signing, the way the SDK's own chunked path does.
    fn modify_before_signing(
        &self,
        _context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _rc: &RuntimeComponents,
        cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        cfg.interceptor_state()
            .store_put(aws_runtime::auth::PayloadSigningOverride::UnsignedPayload);
        Ok(())
    }
}

/// Sends `$ & + , : ; = @` unescaped in the object path, and signs the path
/// that way.
///
/// Escaped, a single one of these anywhere in a key makes the gateway store
/// the whole key escaped (`a&b c` becomes `a%26b%20c`); sent as they are, the
/// key arrives as written. The gateway checks the signature against the path
/// it received, so the change has to come before signing. Only the path
/// changes: in the query `&` and `=` separate parameters.
#[derive(Debug)]
pub struct SendReservedCharsRaw;

impl Intercept for SendReservedCharsRaw {
    fn name(&self) -> &'static str {
        "SendReservedCharsRaw"
    }

    fn modify_before_signing(
        &self,
        context: &mut BeforeTransmitInterceptorContextMut<'_>,
        _rc: &RuntimeComponents,
        _cfg: &mut ConfigBag,
    ) -> Result<(), BoxError> {
        let request = context.request_mut();
        if let Some(uri) = unescape_reserved_in_path(request.uri()) {
            request.set_uri(uri)?;
        }
        Ok(())
    }
}

/// The URI with `%24 %26 %2B %2C %3A %3B %3D %40` in its path turned back into
/// the characters, or `None` when there are none. Any other escape stays, so a
/// name that really contains `%26` still arrives as `%2526`.
fn unescape_reserved_in_path(uri: &str) -> Option<String> {
    let (before_query, query) = match uri.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (uri, None),
    };
    let path_start = before_query
        .find("://")
        .map(|i| i + 3)
        .and_then(|i| before_query[i..].find('/').map(|j| i + j))
        .unwrap_or(0);
    let (head, path) = before_query.split_at(path_start);

    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let escape = rest.get(at..at + 3).unwrap_or(&rest[at..]);
        let raw = match escape.to_ascii_uppercase().as_str() {
            "%24" => Some('$'),
            "%26" => Some('&'),
            "%2B" => Some('+'),
            "%2C" => Some(','),
            "%3A" => Some(':'),
            "%3B" => Some(';'),
            "%3D" => Some('='),
            "%40" => Some('@'),
            _ => None,
        };
        match raw {
            Some(c) => out.push(c),
            None => out.push_str(escape),
        }
        rest = &rest[at + escape.len()..];
    }
    out.push_str(rest);

    if out == path {
        return None;
    }
    Some(match query {
        Some(q) => format!("{head}{out}?{q}"),
        None => format!("{head}{out}"),
    })
}

/// Whether the endpoint is RunPod's gateway.
///
/// Only it needs the raw path: a standard S3 server decodes the escaped
/// characters itself and refuses a signature made over the raw ones.
// Only over https: the RunPod-specific request changes are not applied to plain http.
fn is_runpod(endpoint: &str) -> bool {
    let lower = endpoint.trim().to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    host == "runpod.io" || host.ends_with(".runpod.io")
}

/// Why an endpoint may not carry credentials, or `None` when it may: it must be
/// `https`, or plain `http` to this machine only (a local test server), with no
/// user name or password inside the address. The code is what Settings shows.
pub fn endpoint_problem(endpoint: &str) -> Option<&'static str> {
    let e = endpoint.trim();
    let lower = e.to_ascii_lowercase();
    let (secure, rest) = if let Some(r) = lower.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = lower.strip_prefix("http://") {
        (false, r)
    } else {
        return Some("badEndpoint");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // A query or a fragment has no place in an endpoint, and a fragment would
    // hide a different host from a reader of the address.
    if authority.is_empty()
        || authority.contains('@')
        || e.contains(char::is_whitespace)
        || e.contains(['?', '#'])
    {
        return Some("badEndpoint");
    }
    if secure {
        return None;
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        None
    } else {
        Some("insecureEndpoint")
    }
}

/// Builds a client for the RunPod gateway from credentials given directly.
///
/// Nothing here can fail: a bad endpoint or a wrong key only shows up on the
/// first real request.
pub fn s3_client_from_parts(
    endpoint: &str,
    region: &str,
    bucket: &str,
    access_key: &str,
    secret_key: &str,
) -> (Client, String) {
    let credentials = Credentials::new(access_key, secret_key, None, None, "bg-static");
    let mut config = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(region.to_string()))
        .endpoint_url(endpoint)
        .credentials_provider(credentials)
        .force_path_style(true)
        // One attempt is capped, the whole operation is not: a stalled socket
        // fails instead of hanging forever, while retries and large part
        // uploads still get the time they need.
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(Duration::from_secs(20))
                .operation_attempt_timeout(Duration::from_secs(60))
                .build(),
        )
        // Checksum headers only when an operation requires them.
        .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
        .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
        .interceptor(StripBucketTrailingSlash {
            bucket: bucket.to_string(),
        })
        .interceptor(StripXIdParam)
        .interceptor(DropWriteEntityHeaders)
        .interceptor(FixLastModifiedUtc)
        .interceptor(ForceUnsignedPayload);
    if is_runpod(endpoint) {
        config = config.interceptor(SendReservedCharsRaw);
    }

    (Client::from_conf(config.build()), bucket.to_string())
}

/// Most entries one remote folder's listing may hold.
pub const DIR_MAX_ENTRIES: u64 = 1_000_000;

/// Most entries a listing of everything under a prefix may hold (a folder
/// transfer, a rename, a delete).
pub const TREE_MAX_ENTRIES: u64 = 5_000_000;

/// Guards a listing that follows continuation tokens. A token that comes round
/// again means the server is going in circles, and too many entries means a
/// listing nothing can use; either ends the listing with an error instead of
/// letting it run on or fill memory.
///
/// The SDK's paginator catches only a token repeated twice in a row.
pub struct PageGuard {
    /// Hashes of the tokens seen, so a long token costs no more than a short one.
    seen: std::collections::HashSet<u64>,
    /// The hash of the last token: the same token twice in a row is where the
    /// SDK's paginator itself ends the listing, without an error.
    last: Option<u64>,
    items: u64,
    max_items: u64,
}

impl PageGuard {
    pub fn new(max_items: u64) -> Self {
        PageGuard { seen: Default::default(), last: None, items: 0, max_items }
    }

    /// Counts a page just read: its entries and the token it points on with.
    pub fn page(&mut self, items: usize, next: Option<&str>) -> Result<(), PageStop> {
        use std::hash::{Hash, Hasher};
        self.items += items as u64;
        if self.items > self.max_items {
            return Err(PageStop::TooMany(self.max_items));
        }
        if let Some(token) = next.filter(|t| !t.is_empty()) {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            token.hash(&mut h);
            let h = h.finish();
            if self.last != Some(h) && !self.seen.insert(h) {
                return Err(PageStop::Loop);
            }
            self.last = Some(h);
        }
        Ok(())
    }
}

/// Why a guarded listing stopped.
#[derive(Debug, PartialEq)]
pub enum PageStop {
    /// More entries than the listing may hold.
    TooMany(u64),
    /// A page token came round again.
    Loop,
}

impl std::fmt::Display for PageStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PageStop::TooMany(max) => write!(f, "the listing has more than {max} entries"),
            PageStop::Loop => f.write_str("the server sent a page token it had sent before"),
        }
    }
}

/// Reads a response body into memory, refusing one longer than `cap` bytes: the
/// server decides how much it sends, so the reader decides how much it keeps.
pub async fn read_capped(mut body: ByteStream, cap: u64) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    while let Some(chunk) = body.try_next().await.map_err(|e| e.to_string())? {
        if (out.len() + chunk.len()) as u64 > cap {
            return Err(format!("the response is longer than the {cap} bytes expected"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Turns an SDK error into a line worth reading: the gateway's own code and
/// message when there is one, the error chain otherwise.
pub fn s3_err<E, R>(e: &SdkError<E, R>) -> String
where
    E: ProvideErrorMetadata + Error + 'static,
    R: std::fmt::Debug,
{
    match (e.code(), e.message()) {
        (Some(code), Some(msg)) => format!("{code}: {msg}"),
        (Some(code), None) => code.to_string(),
        _ => DisplayErrorContext(e).to_string(),
    }
}

/// Asks the gateway how large the object actually is.
async fn stored_object_size(client: &Client, bucket: &str, key: &str) -> Result<Option<i64>, String> {
    client
        .head_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .map(|h| h.content_length())
        .map_err(|e| format!("HEAD {key}: {}", s3_err(&e)))
}

/// Checks a finished write and deletes the object if its stored size differs
/// from the source.
///
/// A wrong object is worse than no object: the next listing would show it as a
/// complete file.
pub async fn verify_object_size(
    client: &Client,
    bucket: &str,
    key: &str,
    expected: u64,
) -> Result<(), String> {
    let got = stored_object_size(client, bucket, key).await?;
    if got == Some(expected as i64) {
        return Ok(());
    }
    let _ = client.delete_object().bucket(bucket).key(key).send().await;
    Err(format!(
        "{key}: stored size {} does not match the source ({expected} bytes)",
        got.map_or("unknown".to_string(), |n| n.to_string())
    ))
}

/// Writes a small object in one request and checks its stored size.
pub async fn put_object_verified(
    client: &Client,
    bucket: &str,
    key: &str,
    data: &[u8],
) -> Result<(), String> {
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from(data.to_vec()))
        .send()
        .await
        .map_err(|e| format!("PUT {key}: {}", s3_err(&e)))?;
    verify_object_size(client, bucket, key, data.len() as u64).await
}

#[cfg(test)]
mod interceptor_tests {
    // Each test sends a real SDK operation through the client the app builds, but
    // into a stand-in for the network, and checks what would have gone out.
    use super::*;
    use aws_sdk_s3::primitives::{DateTime, SdkBody};
    use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
    use aws_smithy_http_client::test_util::{CaptureRequestReceiver, capture_request};

    const ENDPOINT: &str = "https://gateway.test";
    const BUCKET: &str = "testbucket";

    const RUNPOD: &str = "https://s3api-eu-ro-1.runpod.io";

    fn capturing_client(response: Option<http::Response<SdkBody>>) -> (Client, CaptureRequestReceiver) {
        capturing_client_at(ENDPOINT, response)
    }

    fn capturing_client_at(
        endpoint: &str,
        response: Option<http::Response<SdkBody>>,
    ) -> (Client, CaptureRequestReceiver) {
        let (client, _) = s3_client_from_parts(endpoint, "eu-ro-1", BUCKET, "AKIDTEST", "not-a-secret");
        let (http_client, rx) = capture_request(response);
        let conf = client.config().to_builder().http_client(http_client).build();
        (Client::from_conf(conf), rx)
    }

    /// The header names the signature covers, from the `Authorization` header.
    fn signed_headers(authorization: &str) -> Vec<&str> {
        authorization
            .split(", ")
            .find_map(|part| part.strip_prefix("SignedHeaders="))
            .expect("a SigV4 authorization header")
            .split(';')
            .collect()
    }

    #[tokio::test]
    async fn put_object_is_signed_the_way_the_gateway_checks_it() {
        let (client, rx) = capturing_client(None);
        client
            .put_object()
            .bucket(BUCKET)
            .key("dir/a.txt")
            .body(ByteStream::from_static(b"a\n"))
            .send()
            .await
            .unwrap();
        let req = rx.expect_request();

        assert_eq!(req.uri(), "https://gateway.test/testbucket/dir/a.txt", "x-id is gone");
        assert_eq!(req.headers().get("x-amz-content-sha256"), Some("UNSIGNED-PAYLOAD"));
        assert_eq!(req.headers().get("content-type"), None);
        let signed = signed_headers(req.headers().get("authorization").unwrap());
        assert!(!signed.contains(&"content-type"), "{signed:?}");
        assert!(!signed.contains(&"content-length"), "{signed:?}");
        assert!(signed.contains(&"x-amz-content-sha256"), "{signed:?}");
    }

    #[tokio::test]
    async fn bucket_listing_loses_the_slash_after_the_bucket() {
        let (client, rx) = capturing_client(None);
        let _ = client
            .list_objects_v2()
            .bucket(BUCKET)
            .delimiter("/")
            .send()
            .await;
        let uri = rx.expect_request().uri().to_string();

        assert!(uri.starts_with("https://gateway.test/testbucket?"), "{uri}");
        assert!(uri.contains("list-type=2"), "{uri}");
    }

    #[tokio::test]
    async fn real_query_parameters_survive_the_x_id_removal() {
        let (client, rx) = capturing_client(None);
        let _ = client
            .upload_part()
            .bucket(BUCKET)
            .key("big.bin")
            .upload_id("u1")
            .part_number(3)
            .body(ByteStream::from_static(b"part"))
            .send()
            .await;
        let uri = rx.expect_request().uri().to_string();

        assert!(!uri.contains("x-id"), "{uri}");
        assert!(uri.contains("partNumber=3"), "{uri}");
        assert!(uri.contains("uploadId=u1"), "{uri}");
    }

    #[tokio::test]
    async fn an_xml_body_keeps_its_content_type() {
        let (client, rx) = capturing_client(None);
        let parts = CompletedMultipartUpload::builder()
            .parts(CompletedPart::builder().part_number(1).e_tag("\"e1\"").build())
            .build();
        let _ = client
            .complete_multipart_upload()
            .bucket(BUCKET)
            .key("big.bin")
            .upload_id("u1")
            .multipart_upload(parts)
            .send()
            .await;
        let req = rx.expect_request();

        let content_type = req.headers().get("content-type").expect("content-type is kept");
        assert_ne!(content_type, "application/octet-stream");
        let signed = signed_headers(req.headers().get("authorization").unwrap());
        assert!(!signed.contains(&"content-length"), "{signed:?}");
    }

    #[tokio::test]
    async fn a_utc_last_modified_is_read() {
        let response = http::Response::builder()
            .status(200)
            .header("last-modified", "Mon, 14 Sep 2026 06:48:03 UTC")
            .header("content-length", "2")
            .body(SdkBody::empty())
            .unwrap();
        let (client, _rx) = capturing_client(Some(response));
        let head = client.head_object().bucket(BUCKET).key("a.txt").send().await.unwrap();

        assert_eq!(head.last_modified(), Some(&DateTime::from_secs(1_789_368_483)));
    }

    async fn put_uri(endpoint: &str, key: &str) -> String {
        let (client, rx) = capturing_client_at(endpoint, None);
        client
            .put_object()
            .bucket(BUCKET)
            .key(key)
            .body(ByteStream::from_static(b""))
            .send()
            .await
            .unwrap();
        rx.expect_request().uri().to_string()
    }

    #[tokio::test]
    async fn runpod_gets_the_eight_characters_raw() {
        let uri = put_uri(RUNPOD, "d&x/a&b+c$d@e=f,g;h:i \u{15f}.txt").await;
        assert_eq!(uri, format!("{RUNPOD}/testbucket/d&x/a&b+c$d@e=f,g;h:i%20%C5%9F.txt"));
    }

    #[tokio::test]
    async fn a_real_percent_sign_stays_escaped() {
        let uri = put_uri(RUNPOD, "100%26 & more.txt").await;
        assert_eq!(uri, format!("{RUNPOD}/testbucket/100%2526%20&%20more.txt"));
    }

    #[tokio::test]
    async fn other_servers_keep_the_standard_escaping() {
        let uri = put_uri(ENDPOINT, "a&b.txt").await;
        assert_eq!(uri, "https://gateway.test/testbucket/a%26b.txt");
    }

    #[tokio::test]
    async fn the_query_keeps_its_escaping() {
        let (client, rx) = capturing_client_at(RUNPOD, None);
        let _ = client
            .list_objects_v2()
            .bucket(BUCKET)
            .prefix("a&b=c/")
            .delimiter("/")
            .send()
            .await;
        let uri = rx.expect_request().uri().to_string();

        assert!(uri.starts_with(&format!("{RUNPOD}/testbucket?")), "{uri}");
        assert!(uri.contains("prefix=a%26b%3Dc%2F"), "{uri}");
    }

    #[test]
    fn only_runpod_hosts_over_https_count_as_runpod() {
        for yes in [RUNPOD, "https://S3API-EU-RO-1.RUNPOD.IO/"] {
            assert!(is_runpod(yes), "{yes}");
        }
        for no in [
            "http://127.0.0.1:9000",
            "https://runpod.io.example.test",
            "https://notrunpod.io",
            "",
            "http://runpod.io:443",
            "http://s3api-eu-ro-1.runpod.io",
            "https://evil.example#.runpod.io",
            "https://evil.example?.runpod.io",
        ] {
            assert!(!is_runpod(no), "{no}");
        }
    }

    #[test]
    fn credentials_go_only_over_https_or_to_this_machine() {
        for ok in [
            RUNPOD,
            "https://s3.example.com:8443/base",
            "HTTPS://S3.EXAMPLE.COM",
            "http://localhost:9000",
            "http://127.0.0.1:19000",
            "http://[::1]:9000",
            "  https://s3.example.com  ",
        ] {
            assert_eq!(endpoint_problem(ok), None, "{ok}");
        }
        for insecure in [
            "http://s3.example.com",
            "http://192.168.122.1:9000",
            "http://localhost.example.com:9000",
            "http://127.0.0.1.nip.io",
            "http://0x7f000001",
        ] {
            assert_eq!(endpoint_problem(insecure), Some("insecureEndpoint"), "{insecure}");
        }
        for bad in [
            "",
            "s3.example.com",
            "ftp://s3.example.com",
            "https://",
            "https://user:pass@s3.example.com",
            "http://127.0.0.1@evil.example",
            "https://s3 example.com",
            "https://evil.example#.runpod.io",
            "https://s3.example.com/?x=1",
        ] {
            assert_eq!(endpoint_problem(bad), Some("badEndpoint"), "{bad}");
        }
    }
}

#[cfg(test)]
mod capped_tests {
    use super::read_capped;
    use aws_sdk_s3::primitives::ByteStream;

    #[tokio::test]
    async fn a_body_longer_than_expected_is_refused() {
        let body = ByteStream::from(vec![7u8; 10]);
        assert_eq!(read_capped(body, 10).await.unwrap().len(), 10);
        let body = ByteStream::from(vec![7u8; 11]);
        assert!(read_capped(body, 10).await.is_err());
    }
}

#[cfg(test)]
mod page_guard_tests {
    use super::PageGuard;

    #[test]
    fn a_token_that_comes_round_again_ends_the_listing() {
        let mut guard = PageGuard::new(u64::MAX);
        assert!(guard.page(1000, Some("A")).is_ok());
        assert!(guard.page(1000, Some("B")).is_ok());
        assert!(guard.page(1000, Some("A")).is_err(), "A came round again");
    }

    // The paginator ends a listing on the same token twice in a row by itself.
    #[test]
    fn a_token_repeated_at_once_is_left_to_the_paginator() {
        let mut guard = PageGuard::new(u64::MAX);
        assert!(guard.page(1000, Some("A")).is_ok());
        assert!(guard.page(1000, Some("A")).is_ok());
    }

    #[test]
    fn a_listing_stops_past_its_entry_budget() {
        let mut guard = PageGuard::new(2500);
        assert!(guard.page(1000, Some("1")).is_ok());
        assert!(guard.page(1000, Some("2")).is_ok());
        assert!(guard.page(1000, None).is_err(), "3000 entries are over 2500");
    }

    #[test]
    fn an_empty_token_is_no_token() {
        let mut guard = PageGuard::new(10);
        assert!(guard.page(3, Some("")).is_ok());
        assert!(guard.page(3, Some("")).is_ok());
    }
}
