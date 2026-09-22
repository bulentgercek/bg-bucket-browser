//! Connection test for the Settings screen.

use std::time::Instant;

use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::operation::list_objects_v2::ListObjectsV2Error;
use serde::Serialize;
use tauri::AppHandle;

use crate::core::s3_client_from_parts;

/// Successful test: round-trip time and the bucket that was reached.
#[derive(Serialize)]
pub struct TestOk {
    pub ms: u64,
    pub bucket: String,
}

/// Failure category; the frontend picks its message from `testError.*` in the locale file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TestErrKind {
    BadSignature,
    UnknownAccessKey,
    AccessDenied,
    NoSuchBucket,
    ServiceError,
    Unreachable,
    Timeout,
    BadResponse,
    BadRequest,
    NoCredentials,
    Unknown,
}

/// Structured test failure: category, the SDK's short message, and the S3
/// error code and HTTP status when the server answered.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestErr {
    pub kind: TestErrKind,
    pub detail: String,
    pub code: Option<String>,
    pub http_status: Option<u16>,
}

// Maps an SDK failure to a category: transport problems by variant, server replies by S3 error code.
fn classify_kind<R>(e: &SdkError<ListObjectsV2Error, R>, code: Option<&str>) -> TestErrKind {
    match e {
        SdkError::ConstructionFailure(_) => TestErrKind::BadRequest,
        SdkError::DispatchFailure(_) => TestErrKind::Unreachable,
        SdkError::TimeoutError(_) => TestErrKind::Timeout,
        SdkError::ResponseError(_) => TestErrKind::BadResponse,
        SdkError::ServiceError(_) => match code {
            Some("SignatureDoesNotMatch") => TestErrKind::BadSignature,
            Some("InvalidAccessKeyId") => TestErrKind::UnknownAccessKey,
            Some("AccessDenied") => TestErrKind::AccessDenied,
            Some("NoSuchBucket") => TestErrKind::NoSuchBucket,
            _ => TestErrKind::ServiceError,
        },
        // The SDK may add variants in future versions.
        _ => TestErrKind::Unknown,
    }
}

/// Connects with the form's values and lists one key to prove access.
///
/// Blank keys fall back to the ones saved in the keychain for this connection,
/// since the form never receives stored secrets.
#[tauri::command]
pub async fn test_connection(
    app: AppHandle,
    id: Option<String>,
    endpoint: String,
    region: String,
    bucket: String,
    access_key: Option<String>,
    secret_key: Option<String>,
) -> Result<TestOk, TestErr> {
    let nonblank = |o: Option<String>| o.filter(|s| !s.trim().is_empty());
    let (saved_ak, saved_sk) =
        crate::config::stored_keys_for(&app, id.as_deref());
    let access_key = nonblank(access_key).or(saved_ak);
    let secret_key = nonblank(secret_key).or(saved_sk);
    let (Some(access_key), Some(secret_key)) = (access_key, secret_key) else {
        return Err(TestErr {
            kind: TestErrKind::NoCredentials,
            detail: "No credentials — enter the access and secret key, or Save first"
                .into(),
            code: None,
            http_status: None,
        });
    };

    let (client, bucket) =
        s3_client_from_parts(&endpoint, &region, &bucket, &access_key, &secret_key);

    // One key is enough to prove the endpoint, signature and bucket all work.
    let started = Instant::now();
    client
        .list_objects_v2()
        .bucket(&bucket)
        .max_keys(1)
        .send()
        .await
        .map_err(|e| {
            let code = e.code().map(str::to_string);
            TestErr {
                kind: classify_kind(&e, code.as_deref()),
                detail: e
                    .message()
                    .map(str::to_string)
                    .unwrap_or_else(|| "No further detail".into()),
                http_status: e.raw_response().map(|r| r.status().as_u16()),
                code,
            }
        })?;

    Ok(TestOk {
        ms: started.elapsed().as_millis() as u64,
        bucket,
    })
}
