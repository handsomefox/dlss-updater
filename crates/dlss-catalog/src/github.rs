use crate::{MAX_ARCHIVE_BYTES, sha256_file};
use dlss_core::{ReleaseId, ReleaseMetadata};
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{ETAG, IF_NONE_MATCH, LINK, USER_AGENT},
};
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const RELEASES_URL: &str =
    "https://api.github.com/repos/NVIDIA-RTX/Streamline/releases?per_page=100";

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct OfficialAsset {
    pub release: ReleaseMetadata,
    pub download_url: String,
    pub size: u64,
    pub digest: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseRefresh {
    NotModified,
    Modified {
        etag: Option<String>,
        assets: Vec<OfficialAsset>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("GitHub returned {status}: {message}")]
    Status { status: StatusCode, message: String },
    #[error("download exceeds safety limit")]
    TooLarge,
    #[error("download digest mismatch")]
    DigestMismatch,
    #[error("invalid SHA-256 digest")]
    InvalidDigest,
    #[error("too many GitHub release pages")]
    TooManyPages,
}

pub struct GithubCatalogClient {
    client: Client,
    releases_url: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DownloadProgress {
    pub received: u64,
    pub total: Option<u64>,
}

impl GithubCatalogClient {
    pub fn new() -> Result<Self, GithubError> {
        // `reqwest/rustls-no-provider` leaves provider selection to the
        // application. Installing ring is idempotent; an error only means a
        // provider was already installed by another client in this process.
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(Self {
            // Release archives are hundreds of megabytes. The blocking client's
            // default 30s timeout covers the whole body read and would abort large
            // downloads, so disable it and bound only connection establishment. The
            // streaming size cap in `download` remains the safety limit.
            client: Client::builder()
                .timeout(None)
                .connect_timeout(Duration::from_secs(30))
                .build()?,
            releases_url: RELEASES_URL.into(),
        })
    }

    #[cfg(test)]
    fn with_releases_url(releases_url: String) -> Result<Self, GithubError> {
        let mut client = Self::new()?;
        client.releases_url = releases_url;
        Ok(client)
    }

    pub fn refresh(&self, previous_etag: Option<&str>) -> Result<ReleaseRefresh, GithubError> {
        tracing::info!(etag = previous_etag, "refreshing GitHub release catalog");
        let mut url = Some(self.releases_url.clone());
        let mut assets = Vec::new();
        let mut etag = None;
        let mut page = 0;
        while let Some(current) = url.take() {
            page += 1;
            if page > 20 {
                return Err(GithubError::TooManyPages);
            }
            let mut request = self.client.get(&current).header(USER_AGENT, "dlss-updater");
            if page == 1
                && let Some(previous_etag) = previous_etag
            {
                request = request.header(IF_NONE_MATCH, previous_etag);
            }
            let response = request.send()?;
            if page == 1 && response.status() == StatusCode::NOT_MODIFIED {
                return Ok(ReleaseRefresh::NotModified);
            }
            if !response.status().is_success() {
                let status = response.status();
                let message = response.text().unwrap_or_default();
                return Err(GithubError::Status { status, message });
            }
            if page == 1 {
                etag = response
                    .headers()
                    .get(ETAG)
                    .and_then(|value| value.to_str().ok())
                    .map(ToOwned::to_owned);
            }
            url = response
                .headers()
                .get(LINK)
                .and_then(|value| value.to_str().ok())
                .and_then(next_link);
            let releases: Vec<ApiRelease> = response.json()?;
            assets.extend(releases.into_iter().filter_map(select_asset));
        }
        assets.sort_by(|left, right| {
            (right.release.published_unix, &right.release.tag)
                .cmp(&(left.release.published_unix, &left.release.tag))
        });
        tracing::info!(releases = assets.len(), "GitHub release catalog refreshed");
        Ok(ReleaseRefresh::Modified { etag, assets })
    }

    pub fn download(
        &self,
        asset: &OfficialAsset,
        destination: &Path,
    ) -> Result<PathBuf, GithubError> {
        self.download_with_progress(asset, destination, |_| {})
    }

    pub fn download_with_progress(
        &self,
        asset: &OfficialAsset,
        destination: &Path,
        mut progress: impl FnMut(DownloadProgress),
    ) -> Result<PathBuf, GithubError> {
        if asset.size > MAX_ARCHIVE_BYTES {
            return Err(GithubError::TooLarge);
        }
        tracing::info!(release = %asset.release.tag, bytes = asset.size, "downloading release archive");
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = destination.with_extension("zip.partial");
        let mut response = self
            .client
            .get(&asset.download_url)
            .header(USER_AGENT, "dlss-updater")
            .send()?;
        if !response.status().is_success() {
            let status = response.status();
            let message = response.text().unwrap_or_default();
            return Err(GithubError::Status { status, message });
        }
        let total = response
            .content_length()
            .or((asset.size > 0).then_some(asset.size));
        if total.is_some_and(|size| size > MAX_ARCHIVE_BYTES) {
            return Err(GithubError::TooLarge);
        }
        progress(DownloadProgress { received: 0, total });
        let result = (|| {
            let mut output = File::create(&temporary)?;
            let mut received = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = response.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                received += read as u64;
                if received > MAX_ARCHIVE_BYTES {
                    return Err(GithubError::TooLarge);
                }
                output.write_all(&buffer[..read])?;
                progress(DownloadProgress { received, total });
            }
            output.flush()?;
            output.sync_all()?;
            if let Some(expected) = asset.digest
                && sha256_file(&temporary)? != expected
            {
                return Err(GithubError::DigestMismatch);
            }
            fs::rename(&temporary, destination)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        tracing::info!(release = %asset.release.tag, path = %destination.display(), "release archive downloaded");
        Ok(destination.to_owned())
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    published_at: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    digest: Option<String>,
}

fn select_asset(release: ApiRelease) -> Option<OfficialAsset> {
    if release.draft || release.prerelease {
        return None;
    }
    let asset = release.assets.into_iter().find(|asset| {
        let name = asset.name.to_ascii_lowercase();
        name.starts_with("streamline-sdk-v") && name.ends_with(".zip")
    })?;
    let digest = asset.digest.as_deref().and_then(parse_digest);
    Some(OfficialAsset {
        release: ReleaseMetadata {
            id: ReleaseId(release.tag_name.clone()),
            tag: release.tag_name,
            asset_name: asset.name,
            published_unix: release
                .published_at
                .parse::<jiff::Timestamp>()
                .map(|timestamp| timestamp.as_second())
                .unwrap_or_default(),
        },
        download_url: asset.browser_download_url,
        size: asset.size,
        digest,
    })
}

fn parse_digest(value: &str) -> Option<[u8; 32]> {
    let value = value.strip_prefix("sha256:")?;
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, output) in digest.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let mut pieces = part.trim().split(';');
        let url = pieces.next()?.trim();
        let relation = pieces.any(|piece| piece.trim() == r#"rel="next""#);
        if relation {
            url.strip_prefix('<')
                .and_then(|url| url.strip_suffix('>'))
                .map(ToOwned::to_owned)
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };
    use tempfile::tempdir;

    fn mock_server(
        build_responses: impl FnOnce(&str) -> Vec<Vec<u8>>,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let responses = build_responses(&base);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    let count = stream.read(&mut buffer).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).into_owned());
                stream.write_all(&response).unwrap();
            }
        });
        (base, requests, handle)
    }

    fn response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
        let mut bytes = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        )
        .into_bytes();
        for (name, value) in headers {
            bytes.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn parses_only_sha256_digests() {
        let text = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(parse_digest(&text), Some([0xab; 32]));
        assert_eq!(parse_digest("sha512:abcd"), None);
        assert_eq!(parse_digest("sha256:not-hex"), None);
    }

    #[test]
    fn follows_only_next_pagination_link() {
        let header =
            r#"<https://api.example/page/1>; rel="prev", <https://api.example/page/3>; rel="next""#;
        assert_eq!(
            next_link(header).as_deref(),
            Some("https://api.example/page/3")
        );
    }

    #[test]
    fn filters_drafts_prereleases_and_unrelated_assets() {
        let release = ApiRelease {
            tag_name: "v2.12.0".into(),
            published_at: "2026-01-02T03:04:05Z".into(),
            draft: false,
            prerelease: false,
            assets: vec![
                ApiAsset {
                    name: "source.zip".into(),
                    browser_download_url: "bad".into(),
                    size: 1,
                    digest: None,
                },
                ApiAsset {
                    name: "streamline-sdk-v2.12.0.zip".into(),
                    browser_download_url: "good".into(),
                    size: 2,
                    digest: None,
                },
            ],
        };
        assert_eq!(select_asset(release).unwrap().download_url, "good");
    }

    #[test]
    fn refresh_uses_etag_pagination_and_explicit_latest_ordering() {
        let (base, requests, server) = mock_server(|base| {
            let page_one = format!(
                r#"[{{"tag_name":"v1","published_at":"2025-01-01T00:00:00Z","draft":false,"prerelease":false,"assets":[{{"name":"streamline-sdk-v1.zip","browser_download_url":"{base}/v1.zip","size":1,"digest":null}}]}}]"#
            );
            let page_two = format!(
                r#"[{{"tag_name":"v2","published_at":"2026-01-01T00:00:00Z","draft":false,"prerelease":false,"assets":[{{"name":"streamline-sdk-v2.zip","browser_download_url":"{base}/v2.zip","size":2,"digest":null}}]}}]"#
            );
            vec![
                response(
                    "200 OK",
                    &[
                        ("ETag", "\"current\"".into()),
                        ("Link", format!("<{base}/page/2>; rel=\"next\"")),
                    ],
                    page_one.as_bytes(),
                ),
                response("200 OK", &[], page_two.as_bytes()),
            ]
        });
        let client = GithubCatalogClient::with_releases_url(format!("{base}/page/1")).unwrap();
        let ReleaseRefresh::Modified { assets, etag } =
            client.refresh(Some("\"previous\"")).unwrap()
        else {
            panic!("expected a modified catalog");
        };
        server.join().unwrap();
        assert_eq!(etag.as_deref(), Some("\"current\""));
        assert_eq!(assets[0].release.tag, "v2");
        let requests = requests.lock().unwrap();
        assert!(requests[0].contains("if-none-match: \"previous\""));
        assert!(!requests[1].contains("if-none-match"));
    }

    #[test]
    fn refresh_honors_not_modified() {
        let (base, _, server) = mock_server(|_| vec![response("304 Not Modified", &[], b"")]);
        let client = GithubCatalogClient::with_releases_url(base).unwrap();
        assert_eq!(
            client.refresh(Some("\"current\"")).unwrap(),
            ReleaseRefresh::NotModified
        );
        server.join().unwrap();
    }

    #[test]
    fn download_enforces_digest_size_and_reports_progress() {
        let body = b"validated archive bytes";
        let expected: [u8; 32] = Sha256::digest(body).into();
        let (base, _, server) = mock_server(|_| vec![response("200 OK", &[], body)]);
        let asset = OfficialAsset {
            release: ReleaseMetadata {
                id: ReleaseId("v1".into()),
                tag: "v1".into(),
                asset_name: "streamline-sdk-v1.zip".into(),
                published_unix: 1,
            },
            download_url: base,
            size: body.len() as u64,
            digest: Some(expected),
        };
        let directory = tempdir().unwrap();
        let destination = directory.path().join("archive.zip");
        let mut updates = Vec::new();
        GithubCatalogClient::new()
            .unwrap()
            .download_with_progress(&asset, &destination, |progress| updates.push(progress))
            .unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(destination).unwrap(), body);
        assert_eq!(updates.last().unwrap().received, body.len() as u64);

        let too_large = OfficialAsset {
            size: MAX_ARCHIVE_BYTES + 1,
            ..asset.clone()
        };
        assert!(matches!(
            GithubCatalogClient::new()
                .unwrap()
                .download(&too_large, &directory.path().join("too-large.zip")),
            Err(GithubError::TooLarge)
        ));

        let (base, _, server) = mock_server(|_| vec![response("200 OK", &[], body)]);
        let wrong_digest = OfficialAsset {
            download_url: base,
            digest: Some([0; 32]),
            ..asset
        };
        assert!(matches!(
            GithubCatalogClient::new()
                .unwrap()
                .download(&wrong_digest, &directory.path().join("wrong.zip")),
            Err(GithubError::DigestMismatch)
        ));
        server.join().unwrap();
    }
}
