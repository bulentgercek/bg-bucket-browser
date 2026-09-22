//! Total capacity of the active network volume, from the RunPod account API.
//!
//! S3 has no notion of capacity, so this uses RunPod's GraphQL API with the
//! account-level API key, separate from the S3 credentials. The API reports
//! only the total size; used and free space are not available.

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::config::{active_client, runpod_api_key};

const GRAPHQL_URL: &str = "https://api.runpod.io/graphql";

#[derive(Serialize)]
struct GqlBody {
    query: &'static str,
}

// Only the fields this module reads; serde ignores the rest of the response.
#[derive(Deserialize)]
struct GqlResponse {
    #[serde(default)]
    data: Option<GqlData>,
    #[serde(default)]
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct GqlData {
    myself: Myself,
}

#[derive(Deserialize)]
struct Myself {
    #[serde(rename = "networkVolumes")]
    network_volumes: Vec<NetworkVolume>,
}

#[derive(Deserialize)]
struct NetworkVolume {
    id: String,
    size: i64,
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct VolumeQuota {
    pub total_bytes: i64,
}

/// Total size in bytes of the active connection's network volume.
///
/// Fails when no API key is set or the volume is not on the account; the
/// status bar then shows the connection without a capacity.
#[tauri::command]
pub async fn volume_quota(app: AppHandle) -> Result<VolumeQuota, String> {
    let api_key = runpod_api_key().ok_or_else(|| "RunPod API key not set".to_string())?;
    let (_, bucket) = active_client(&app).map_err(|e| e.to_string())?;

    let body = GqlBody {
        query: "query { myself { networkVolumes { id size } } }",
    };
    // Bounded so a stalled request cannot leave the status bar waiting forever.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    // The key goes in a header: URLs end up in server logs and in error messages.
    let resp = client
        .post(GRAPHQL_URL)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json::<GqlResponse>()
        .await
        .map_err(|e| e.to_string())?;

    // GraphQL reports failures in the body with HTTP 200, so check `errors` first.
    if let Some(errs) = resp.errors {
        return Err(errs
            .into_iter()
            .map(|e| e.message)
            .collect::<Vec<_>>()
            .join("; "));
    }
    let volumes = resp
        .data
        .ok_or_else(|| "empty RunPod API response".to_string())?
        .myself
        .network_volumes;
    // On RunPod the S3 bucket name is the network volume ID.
    let vol = volumes
        .into_iter()
        .find(|v| v.id == bucket)
        .ok_or_else(|| format!("network volume '{bucket}' not found on this RunPod account"))?;

    // RunPod reports `size` in decimal GB (10^9 bytes), not GiB.
    Ok(VolumeQuota {
        total_bytes: vol.size.saturating_mul(1_000_000_000),
    })
}
