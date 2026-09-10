use super::*;
use std::{io::Read, process::Stdio, sync::Mutex};

const SAMPLE_LIMIT: usize = 512 * 1024;
static BENCHMARK_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MirrorProbeStatus {
    Ok,
    Timeout,
    HttpError,
    NetworkError,
    InvalidIndex,
    SampleTooLarge,
    CurlMissing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorProbe {
    pub kind: SourceKind,
    pub url: String,
    pub status: MirrorProbeStatus,
    pub http_status: Option<u16>,
    pub latency_ms: Option<f64>,
    pub bytes_per_second: Option<f64>,
    pub downloaded_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorBenchmark {
    pub provider_id: String,
    pub source_revision: String,
    pub collected_at: DateTime<Utc>,
    pub synthetic: bool,
    pub probes: Vec<MirrorProbe>,
}

impl SourceManager {
    /// Read-only, bounded index samples. Never refreshes APT or changes its files.
    pub(crate) fn benchmark(&self, provider_id: &str) -> Result<MirrorBenchmark, SourceError> {
        let provider = PROVIDERS
            .iter()
            .find(|item| item.id == provider_id)
            .copied()
            .ok_or_else(|| SourceError::UnknownProvider(provider_id.into()))?;
        let status = self.status()?;
        if !status.supported {
            return Err(SourceError::Unsupported("no supported APT sources".into()));
        }
        // Bound concurrent requests across all controllers in this process. No queued tests.
        let _guard = BENCHMARK_LOCK
            .try_lock()
            .map_err(|_| SourceError::Io("mirror benchmark is busy; retry later".into()))?;
        let documents = self.documents()?;
        let targets = benchmark_targets(
            &documents,
            &status.distribution_id,
            &status.architecture,
            provider,
        );
        let probes = targets
            .into_iter()
            .map(|(kind, url)| {
                if self.synthetic {
                    let seed = PROVIDERS
                        .iter()
                        .position(|item| item.id == provider_id)
                        .unwrap_or(0);
                    MirrorProbe {
                        kind,
                        url,
                        status: MirrorProbeStatus::Ok,
                        http_status: Some(200),
                        latency_ms: Some(35.0 + seed as f64 * 17.0),
                        bytes_per_second: Some(196_608.0 / (0.18 + seed as f64 * 0.09)),
                        downloaded_bytes: 196_608,
                    }
                } else {
                    probe(kind, url)
                }
            })
            .collect();
        Ok(MirrorBenchmark {
            provider_id: provider_id.into(),
            source_revision: source_revision(&documents),
            collected_at: Utc::now(),
            synthetic: self.synthetic,
            probes,
        })
    }
}

fn safe_segment(value: &str) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

// Inspect enabled binary repositories, preserving suites and the Radxa archive suffix.
// Probe one system index and one Radxa index, not every pocket or component.
fn benchmark_targets(
    documents: &[SourceDocument],
    distribution: &str,
    architecture: &str,
    provider: ProviderDefinition,
) -> Vec<(SourceKind, String)> {
    let mut candidates = Vec::new();
    let mut add = |uri: &str, suite: &str| {
        if !safe_segment(suite) {
            return;
        }
        let Some((kind, _, suffix)) = classify_uri(uri, distribution) else {
            return;
        };
        if !suffix.is_empty() && !suffix.split('/').all(safe_segment) {
            return;
        }
        if let Some(base) = target_uri(
            provider,
            kind,
            distribution,
            architecture,
            suite.ends_with("-security"),
            &suffix,
        ) {
            let candidate = (kind, format!("{base}/dists/{suite}/InRelease"));
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    };
    for document in documents {
        if document.format == "list" {
            for line in document
                .content
                .lines()
                .filter(|line| line.split_whitespace().next() == Some("deb"))
            {
                if let Some((_, end, uri)) = uri_tokens(line).first().copied() {
                    if let Some(suite) = line[end..].split_whitespace().next() {
                        add(uri, suite);
                    }
                }
            }
        } else {
            let normalized = document
                .content
                .lines()
                .map(|line| if line.trim().is_empty() { "" } else { line })
                .collect::<Vec<_>>()
                .join("\n");
            for block in normalized.split("\n\n") {
                let mut fields = std::collections::BTreeMap::<String, String>::new();
                let mut previous = String::new();
                for line in block
                    .lines()
                    .filter(|line| !line.trim_start().starts_with('#'))
                {
                    if line.starts_with([' ', '\t']) {
                        if let Some(value) = fields.get_mut(&previous) {
                            value.push(' ');
                            value.push_str(line.trim());
                        }
                    } else if let Some((key, value)) = line.split_once(':') {
                        previous = key.trim().to_ascii_lowercase();
                        fields.insert(previous.clone(), value.trim().into());
                    }
                }
                if fields
                    .get("enabled")
                    .is_some_and(|value| value.eq_ignore_ascii_case("no"))
                    || !fields
                        .get("types")
                        .is_some_and(|value| value.split_whitespace().any(|value| value == "deb"))
                {
                    continue;
                }
                if let (Some(uris), Some(suites)) = (fields.get("uris"), fields.get("suites")) {
                    for uri in uris.split_whitespace() {
                        for suite in suites.split_whitespace() {
                            add(uri, suite);
                        }
                    }
                }
            }
        }
    }
    candidates.sort_by_key(|(kind, _)| *kind);
    let system = candidates
        .iter()
        .find(|(kind, _)| *kind != SourceKind::Radxa)
        .cloned();
    let radxa = candidates
        .into_iter()
        .find(|(kind, _)| *kind == SourceKind::Radxa);
    system.into_iter().chain(radxa).collect()
}

fn curl_command(url: &str) -> Command {
    let mut command = Command::new("curl");
    // Ignore curlrc, keep TLS verification on, and never follow server redirects.
    command.args([
        "--disable",
        "--silent",
        "--fail",
        "--globoff",
        "--proto",
        "=https",
        "--connect-timeout",
        "2",
        "--max-time",
        "6",
        "--range",
        "0-524287",
        "--write-out",
        "%{stderr}%{http_code} %{time_starttransfer} %{speed_download}",
        "--url",
        url,
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn probe(kind: SourceKind, url: String) -> MirrorProbe {
    let command = curl_command(&url);
    probe_with_command(kind, url, command)
}

fn probe_with_command(kind: SourceKind, url: String, mut command: Command) -> MirrorProbe {
    let mut result = MirrorProbe {
        kind,
        url,
        status: MirrorProbeStatus::NetworkError,
        http_status: None,
        latency_ms: None,
        bytes_per_second: None,
        downloaded_bytes: 0,
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                result.status = MirrorProbeStatus::CurlMissing;
            }
            return result;
        }
    };
    let mut body = Vec::new();
    let read = child
        .stdout
        .take()
        .expect("piped stdout")
        .take((SAMPLE_LIMIT + 1) as u64)
        .read_to_end(&mut body);
    // Bound memory/body consumption even when the server ignores Range or uses chunked encoding.
    if body.len() > SAMPLE_LIMIT || read.is_err() {
        let _ = child.kill();
    }
    let output = child.wait_with_output();
    result.downloaded_bytes = body.len().min(SAMPLE_LIMIT);
    if body.len() > SAMPLE_LIMIT {
        result.status = MirrorProbeStatus::SampleTooLarge;
        return result;
    }
    let Ok(output) = output else {
        return result;
    };
    if read.is_err() {
        return result;
    }
    let metrics = String::from_utf8_lossy(&output.stderr);
    let values = metrics.split_whitespace().collect::<Vec<_>>();
    result.http_status = values
        .first()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0);
    if !output.status.success() {
        result.status = match output.status.code() {
            Some(28) => MirrorProbeStatus::Timeout,
            Some(22) => MirrorProbeStatus::HttpError,
            _ => MirrorProbeStatus::NetworkError,
        };
        return result;
    }
    if !matches!(result.http_status, Some(200 | 206)) {
        result.status = MirrorProbeStatus::HttpError;
        return result;
    }
    let Some((latency, speed)) = parse_metrics(&values) else {
        return result;
    };
    if !body.starts_with(b"-----BEGIN PGP SIGNED MESSAGE-----") {
        result.status = MirrorProbeStatus::InvalidIndex;
        return result;
    }
    result.status = MirrorProbeStatus::Ok;
    result.latency_ms = Some(latency * 1000.0);
    result.bytes_per_second = Some(speed);
    result
}

fn parse_metrics(values: &[&str]) -> Option<(f64, f64)> {
    let latency: f64 = values.get(1)?.parse().ok()?;
    let speed: f64 = values.get(2)?.parse().ok()?;
    (latency.is_finite() && latency >= 0.0 && speed.is_finite() && speed > 0.0)
        .then_some((latency, speed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    #[ignore = "requires curl and loopback sockets; run explicitly in CI"]
    fn curl_benchmark_fixture() {
        use std::{net::TcpListener, time::Duration};
        for (status, body, expected) in [
            (
                "200 OK",
                b"-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nSuite: bookworm\n".to_vec(),
                MirrorProbeStatus::Ok,
            ),
            (
                "206 Partial Content",
                b"-----BEGIN PGP SIGNED MESSAGE-----\nSuite: bookworm\n".to_vec(),
                MirrorProbeStatus::Ok,
            ),
            (
                "200 OK",
                b"<html>captive portal</html>".to_vec(),
                MirrorProbeStatus::InvalidIndex,
            ),
            ("404 Not Found", vec![], MirrorProbeStatus::HttpError),
            ("302 Found", vec![], MirrorProbeStatus::HttpError),
            (
                "200 OK",
                vec![b'x'; SAMPLE_LIMIT + 4096],
                MirrorProbeStatus::SampleTooLarge,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/InRelease", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut request = [0; 4096];
                let received = stream.read(&mut request).unwrap();
                assert!(received > 0);
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nLocation: https://example.invalid/never-follow\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            });
            let mut command = curl_command(&url);
            // Only this fixture allows HTTP and bypasses environment proxies.
            command.args(["--proto", "=http", "--noproxy", "*"]);
            let result = probe_with_command(SourceKind::Debian, url, command);
            server.join().unwrap();
            assert_eq!(result.status, expected);
            assert!(result.downloaded_bytes <= SAMPLE_LIMIT);
            assert_eq!(
                result.latency_ms.is_some(),
                expected == MirrorProbeStatus::Ok
            );
            assert_eq!(
                result.bytes_per_second.is_some(),
                expected == MirrorProbeStatus::Ok
            );
        }
        let result = probe_with_command(
            SourceKind::Debian,
            "https://example.invalid".into(),
            Command::new("/nonexistent/rsetup-test-curl"),
        );
        assert_eq!(result.status, MirrorProbeStatus::CurlMissing);
    }

    fn doc(format: &'static str, content: &str) -> SourceDocument {
        SourceDocument {
            actual_path: None,
            display_path: "test".into(),
            format,
            content: content.into(),
        }
    }

    #[test]
    fn targets_preserve_suites_and_radxa_suffix() {
        let documents = SourceManager::new(true).documents().unwrap();
        assert_eq!(
            benchmark_targets(&documents, "debian", "aarch64", PROVIDERS[3]),
            vec![
                (
                    SourceKind::Debian,
                    "https://mirrors.cqu.edu.cn/debian/dists/bookworm/InRelease".into()
                ),
                (
                    SourceKind::Radxa,
                    "https://mirrors.cqu.edu.cn/radxa-deb/bookworm/dists/bookworm/InRelease".into()
                )
            ]
        );
        assert_eq!(
            benchmark_targets(&documents, "debian", "aarch64", PROVIDERS[1]).len(),
            1
        );
        assert_eq!(
            benchmark_targets(&documents, "debian", "aarch64", PROVIDERS[9])[0].0,
            SourceKind::Radxa
        );
    }

    #[test]
    fn disabled_source_only_third_party_and_unsafe_paths_are_skipped() {
        let documents = vec![
            doc(
                "deb822",
                "Types: deb\nEnabled: no\nURIs: https://deb.debian.org/debian\nSuites: bookworm\n\nTypes: deb-src\nURIs: https://deb.debian.org/debian\nSuites: bookworm\n",
            ),
            doc(
                "list",
                "# deb https://deb.debian.org/debian bookworm main\ndeb https://example.com/debian bookworm main\ndeb https://deb.debian.org/debian ../../private main\ndeb https://radxa-repo.github.io/../private bookworm main\n",
            ),
        ];
        assert!(benchmark_targets(&documents, "debian", "aarch64", PROVIDERS[0]).is_empty());
    }

    #[test]
    fn ubuntu_ports_and_deb822_continuations() {
        let documents = vec![doc(
            "deb822",
            "Types: deb\nURIs:\n https://ports.ubuntu.com/ubuntu-ports\nSuites:\n noble noble-updates\n",
        )];
        assert_eq!(
            benchmark_targets(&documents, "ubuntu", "aarch64", PROVIDERS[0])[0].1,
            "https://ports.ubuntu.com/ubuntu-ports/dists/noble/InRelease"
        );
    }

    #[test]
    fn demo_benchmark_is_explicit_and_does_not_change_sources() {
        let manager = SourceManager::new(true);
        let before = manager.status().unwrap().source_revision;
        let result = manager.benchmark("cqu").unwrap();
        assert!(result.synthetic);
        assert_eq!(result.probes.len(), 2);
        assert_eq!(before, result.source_revision);
        assert_eq!(before, manager.status().unwrap().source_revision);
        assert!(matches!(
            manager.benchmark("https://localhost"),
            Err(SourceError::UnknownProvider(_))
        ));
    }

    #[test]
    fn curl_limits_and_metrics_are_safe() {
        let command = curl_command("https://deb.debian.org/debian/dists/bookworm/InRelease");
        let args = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(args[0], "--disable");
        assert!(args.windows(2).any(|pair| pair == ["--max-time", "6"]));
        assert!(args.windows(2).any(|pair| pair == ["--proto", "=https"]));
        assert!(!args.contains(&"--location"));
        assert!(!args.contains(&"--insecure"));
        assert_eq!(parse_metrics(&["200", "0.1", "1024"]), Some((0.1, 1024.0)));
        assert!(parse_metrics(&["200", "NaN", "1024"]).is_none());
        assert!(parse_metrics(&["200", "0.1", "0"]).is_none());
    }
}
