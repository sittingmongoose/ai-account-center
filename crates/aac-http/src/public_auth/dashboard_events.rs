//! The public dashboard event contract and the private producer pipe.
use axum::body::Body;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use std::io;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    sync::broadcast,
};

const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Push hints sent over the existing dashboard WebSocket.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DashboardEvent {
    /// Account usage or account configuration changed.
    AccountsChanged {},
    /// A Status source changed.
    StatusChanged {},
    /// Usage history or activity for the Analytics view changed.
    AnalyticsChanged {},
    /// Reset claims changed.
    ResetClaimsChanged {
        /// Opaque identifier for this change.
        id: String,
    },
    /// Progress from the update owner without install output or result diagnostics.
    AppUpdateJob {
        /// Current job progress.
        job: AppUpdateProgress,
    },
    /// A state transition from the sign-in owner.
    SigninJob {
        /// The sign-in route's public DTO.
        job: Box<DashboardSignInJob>,
    },
}

/// Public Update all progress. Detailed results are read through the guarded HTTP route.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppUpdateProgress {
    /// Job id.
    pub id: String,
    /// Running, completed or failed.
    pub state: String,
    /// Whether the owner acknowledged cancellation.
    pub cancel_requested: bool,
    /// Number of app results already available.
    pub completed_results: u32,
    /// Number expected at completion, if known.
    pub expected_results: Option<u32>,
    /// Current progress of each participating host.
    pub hosts: Vec<AppUpdateHostProgress>,
}

/// One host's progress within an Update all job.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppUpdateHostProgress {
    /// Dashboard host id.
    pub host: String,
    /// Waiting, running or done.
    pub state: String,
    /// App under consideration, if any.
    pub current_app: Option<String>,
    /// Checking, updating or downloading, if known.
    pub phase: Option<String>,
    /// When the current phase began.
    pub phase_since: Option<String>,
}

/// Public sign-in job fields shared with the HTTP job DTO.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DashboardSignInJob {
    /// Job id.
    pub id: String,
    /// Account provider.
    pub provider: String,
    /// Device code or supervised CLI.
    pub kind: String,
    /// Add or sign in again.
    pub mode: String,
    /// Target account id, if known.
    pub account_id: Option<String>,
    /// Target profile, if named.
    pub profile_name: Option<String>,
    /// Host platform.
    pub platform: String,
    /// Job state.
    pub state: String,
    /// Verification data, removed for plain transports.
    pub verification: Option<SignInVerification>,
    /// Final result, with email removed for plain transports.
    pub result: Option<SignInResult>,
    /// Stable error details, if the job failed.
    pub error: Option<SignInError>,
    /// Start timestamp.
    pub started_at: String,
    /// Last transition timestamp.
    pub updated_at: String,
    /// Expiry timestamp.
    pub expires_at: String,
}

/// Browser verification step from the sign-in owner.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInVerification {
    /// Verification URL.
    pub url: String,
    /// One-time device code, if the job has one.
    pub user_code: Option<String>,
    /// Expiry for the verification step, if known.
    pub expires_at: Option<String>,
}

/// Saved sign-in result.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignInResult {
    /// Account id saved by the job.
    pub account_id: String,
    /// Reported email address, if known.
    pub email: Option<String>,
    /// Reported plan, if known.
    pub plan: Option<String>,
}

/// Sign-in failure DTO.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignInError {
    /// Stable error code.
    pub code: String,
    /// Public error message.
    pub message: String,
}

impl DashboardSignInJob {
    pub(super) fn redact(&mut self) {
        self.verification = None;
        if let Some(result) = &mut self.result {
            result.email = None;
        }
    }
}

impl DashboardEvent {
    pub(super) fn for_client(mut self, session: bool, secure: bool) -> Option<Self> {
        if !session && !matches!(self, Self::AccountsChanged {}) {
            return None;
        }
        if !secure && let Self::SigninJob { job } = &mut self {
            job.redact();
        }
        Some(self)
    }
}

/// Publisher shared by Rust owners and the inherited producer feed.
#[derive(Clone)]
pub struct DashboardEvents(pub(super) broadcast::Sender<DashboardEvent>);

impl DashboardEvents {
    /// Publish to current subscribers. Private details are filtered per connection at delivery.
    pub fn publish(&self, event: DashboardEvent) -> usize {
        if let DashboardEvent::ResetClaimsChanged { id } = &event
            && (id.len() != 32
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        {
            return 0;
        }
        if !serde_json::to_vec(&event).is_ok_and(|frame| frame.len() < MAX_FRAME_BYTES) {
            return 0;
        }
        self.0.send(event).unwrap_or(0)
    }

    /// Generate one opaque identifier and publish a reset-claims change.
    pub fn reset_claims_changed(&self) -> usize {
        let Ok(bytes) = super::storage::random::<16>() else {
            return 0;
        };
        self.publish(DashboardEvent::ResetClaimsChanged {
            id: super::storage::hex(&bytes),
        })
    }

    /// Read the supervisor's private newline-delimited JSON stream.
    /// Malformed frames are skipped. An oversized or unterminated frame closes this feed.
    /// The caller supplies a private stream, such as the child's inherited stdin.
    pub async fn read_feed(&self, input: impl AsyncRead + Unpin) -> io::Result<()> {
        self.read_frames(input, |frame| serde_json::from_slice(frame).ok())
            .await
    }

    /// Bridge only the Status hint from the existing private service feeds.
    pub(super) async fn read_status_feed(&self, body: Body) -> io::Result<()> {
        let stream = body.into_data_stream().map_err(io::Error::other);
        self.read_frames(tokio_util::io::StreamReader::new(stream), |frame| {
            let event = serde_json::from_slice::<serde_json::Value>(frame).ok()?;
            (event["event"] == "status-changed").then_some(DashboardEvent::StatusChanged {})
        })
        .await
    }

    async fn read_frames(
        &self,
        input: impl AsyncRead + Unpin,
        parse: impl Fn(&[u8]) -> Option<DashboardEvent>,
    ) -> io::Result<()> {
        let mut input = BufReader::new(input);
        let mut frame = Vec::new();
        loop {
            frame.clear();
            let read = (&mut input)
                .take(MAX_FRAME_BYTES as u64 + 1)
                .read_until(b'\n', &mut frame)
                .await?;
            if read == 0 {
                return Ok(());
            }
            if read > MAX_FRAME_BYTES || frame.last() != Some(&b'\n') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid event frame boundary",
                ));
            }
            if let Some(event) = parse(&frame) {
                self.publish(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::{Value, json};

    const JOB: &str = r#"{"type":"signin-job","job":{"id":"job_0123456789abcdef","provider":"codex","kind":"device-code","mode":"add","accountId":null,"profileName":null,"platform":"ubuntu","state":"waiting","verification":{"url":"https://example.test/verify","userCode":"test-code","expiresAt":null},"result":{"accountId":"fixture","email":"user@example.test","plan":null},"error":null,"startedAt":"start","updatedAt":"update","expiresAt":"expiry"}}"#;

    #[test]
    fn role_and_transport_filter_the_existing_wire_dto() {
        let event: DashboardEvent = serde_json::from_str(JOB).unwrap();
        assert_eq!(
            serde_json::to_string(&event.clone().for_client(true, true).unwrap()).unwrap(),
            JOB
        );
        assert!(event.clone().for_client(false, true).is_none());
        let plain = serde_json::to_value(event.for_client(true, false).unwrap()).unwrap();
        assert_eq!(plain["job"]["verification"], Value::Null);
        assert_eq!(
            plain["job"]["result"],
            json!({"accountId":"fixture","email":null,"plan":null})
        );
        assert!(
            DashboardEvent::StatusChanged {}
                .for_client(false, true)
                .is_none()
        );
        assert!(
            DashboardEvent::AnalyticsChanged {}
                .for_client(false, true)
                .is_none()
        );
        assert!(
            DashboardEvent::AccountsChanged {}
                .for_client(false, false)
                .is_some()
        );
    }

    #[tokio::test]
    async fn pipe_refuses_unknown_fields_and_partial_or_oversized_frames() {
        let events = DashboardEvents(broadcast::channel(8).0);
        let mut frames = events.0.subscribe();
        let input =
            b"{\"type\":\"status-changed\",\"extra\":true}\n{\"type\":\"analytics-changed\"}\n";
        events.read_feed(&input[..]).await.unwrap();
        assert!(matches!(
            frames.try_recv().unwrap(),
            DashboardEvent::AnalyticsChanged {}
        ));
        assert!(frames.try_recv().is_err());
        let input = b"{\"type\":\"status-changed\"}";
        assert!(events.read_feed(&input[..]).await.is_err());
        let oversized = vec![b' '; MAX_FRAME_BYTES + 1];
        assert!(events.read_feed(&oversized[..]).await.is_err());
        assert!(frames.try_recv().is_err());
    }
}
