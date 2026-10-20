use serde::{Deserialize, Serialize};

pub const CONTROL_ALPN: &[u8] = b"altsendme/control/1";

pub const AUTH_LABEL: &[u8] = b"altsendme-device-auth-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ControlMessage {
    PairingInfo {
        endpoint_id: String,
        display_name: String,
        device_type: String,
        /// OS family exchanged at pair time (`macos`, `linux`, …). Optional for
        /// backward compatibility with older peers.
        #[serde(default)]
        os: String,
        signature: String,
    },
    RememberVote {
        session_id: String,
        vote: RememberVote,
    },
    Invite {
        blob_ticket: String,
        file_count: u32,
        total_size: u64,
        sender_name: String,
    },
    InviteResponse {
        session_id: String,
        response: InviteResponse,
    },
    Recognition {
        signature: String,
    },
    Forget {
        signature: String,
    },
    /// Asks an unpaired LAN peer to identify itself, mDNS carries only node
    /// ids and addresses, so this is the only source of a readable name.
    WhoAreYou,
    /// Reply to `WhoAreYou`. Unsigned and self-reported, the trust anchor is
    /// the connection's public-key binding, shown as `short_fingerprint`.
    Identity {
        endpoint_id: String,
        display_name: String,
        device_type: String,
        /// Optional for parity with `PairingInfo`.
        #[serde(default)]
        os: String,
    },
    /// Asks an unpaired LAN peer to become a paired contact (no file share).
    /// Accept replies with `InviteResponse` and both sides commit records, as a
    /// nearby file-invite accept does.
    PairRequest {
        sender_name: String,
        device_type: String,
        #[serde(default)]
        os: String,
    },
    /// One-off share with a device we don't pair with: the receiver is asked
    /// to accept this download only, and no record of either side is kept.
    /// `id` names it in a `QuickShareDeclined` answer.
    QuickShare {
        id: String,
        blob_ticket: String,
        file_count: u32,
        total_size: u64,
        /// "report.pdf", "3 items", "text"
        title: String,
        sender_name: String,
        #[serde(default)]
        device_type: String,
    },
    /// The receiver turned a `QuickShare` down (accepting is the download).
    QuickShareDeclined { id: String },
    /// One message of the Automerge sync protocol for the document shared
    /// with a paired peer (base64). Paired peers only.
    DocSync { data: String },
}

impl ControlMessage {
    /// Stable label for diagnostics, matching the serde tag. The variant name
    /// and nothing else, payloads carry names, tickets and signatures.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PairingInfo { .. } => "pairing-info",
            Self::RememberVote { .. } => "remember-vote",
            Self::Invite { .. } => "invite",
            Self::InviteResponse { .. } => "invite-response",
            Self::Recognition { .. } => "recognition",
            Self::Forget { .. } => "forget",
            Self::WhoAreYou => "who-are-you",
            Self::Identity { .. } => "identity",
            Self::PairRequest { .. } => "pair-request",
            Self::QuickShare { .. } => "quick-share",
            Self::QuickShareDeclined { .. } => "quick-share-declined",
            Self::DocSync { .. } => "doc-sync",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RememberVote {
    Remember,
    No,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InviteResponse {
    Accepted,
    Declined,
}

/// Pairing join payload encoded in QR / paste code.
///
/// Encoded as a bare 64-char endpoint id when no relay hint is needed; JSON
/// with `relay_url` when the host uses a custom relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingTicket {
    #[serde(default = "default_v", skip_serializing_if = "is_v1")]
    pub v: u32,
    pub kind: String,
    pub endpoint_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
}

const fn default_v() -> u32 {
    1
}

const fn is_v1(v: &u32) -> bool {
    *v == 1
}

fn is_endpoint_id_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Relay URL to embed in a pairing ticket. Public/default relays are omitted
/// because joiners discover them via Pkarr/DNS.
pub fn pairing_ticket_relay_hint(relay_url: Option<String>) -> Option<String> {
    relay_url.filter(|url| !crate::protocol::relay::is_public_relay_url(url))
}

impl PairingTicket {
    pub const KIND: &'static str = "pair";

    pub fn encode(&self) -> anyhow::Result<String> {
        let relay_hint = pairing_ticket_relay_hint(self.relay_url.clone());
        if relay_hint.is_none() {
            anyhow::ensure!(
                is_endpoint_id_hex(&self.endpoint_id),
                "invalid endpoint id"
            );
            return Ok(self.endpoint_id.clone());
        }
        let ticket = Self {
            v: self.v,
            kind: self.kind.clone(),
            endpoint_id: self.endpoint_id.clone(),
            relay_url: relay_hint,
        };
        Ok(serde_json::to_string(&ticket)?)
    }

    pub fn decode(s: &str) -> anyhow::Result<Self> {
        let trimmed = s.trim();
        if let Ok(ticket) = serde_json::from_str::<Self>(trimmed) {
            anyhow::ensure!(ticket.kind == Self::KIND, "not a pairing ticket");
            // Parse with iroh so validation matches what join accepts.
            anyhow::ensure!(
                ticket.endpoint_id.parse::<iroh::EndpointId>().is_ok(),
                "invalid endpoint id in pairing ticket"
            );
            return Ok(ticket);
        }
        // Allow bare endpoint id hex for manual entry.
        if is_endpoint_id_hex(trimmed) {
            anyhow::ensure!(
                trimmed.parse::<iroh::EndpointId>().is_ok(),
                "invalid endpoint id in pairing ticket"
            );
            return Ok(Self {
                v: 1,
                kind: Self::KIND.to_string(),
                endpoint_id: trimmed.to_string(),
                relay_url: None,
            });
        }
        anyhow::bail!("invalid pairing ticket")
    }
}

/// Largest control message, except a document sync.
const MAX_CONTROL: usize = 1024 * 1024;
/// Largest document sync (base64 of an Automerge sync message): a first sync
/// carries the whole history.
const MAX_DOC_SYNC: usize = 32 * 1024 * 1024;
/// How a serialized `DocSync` begins (serde writes the tag first).
const DOC_SYNC_PREFIX: &[u8] = br#"{"type":"doc-sync""#;

pub async fn write_message(
    send: &mut (impl tokio::io::AsyncWrite + Unpin),
    message: &ControlMessage,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let body = serde_json::to_vec(message)?;
    let max = if matches!(message, ControlMessage::DocSync { .. }) {
        MAX_DOC_SYNC
    } else {
        MAX_CONTROL
    };
    anyhow::ensure!(body.len() <= max, "control message too large");
    tracing::debug!(
        target: "dashbeam::_events::control::msg_out",
        kind = message.kind(),
        bytes = body.len(),
    );
    let len = (body.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(&body).await?;
    send.flush().await?;
    Ok(())
}

pub async fn read_message(
    recv: &mut (impl tokio::io::AsyncRead + Unpin),
) -> anyhow::Result<ControlMessage> {
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    anyhow::ensure!(len > 0 && len <= MAX_DOC_SYNC, "invalid control message length");
    let mut body = vec![0u8; len];
    if len > MAX_CONTROL {
        // Only a document sync may be this large: check its tag before
        // reading the rest.
        let head = DOC_SYNC_PREFIX.len().min(len);
        recv.read_exact(&mut body[..head]).await?;
        anyhow::ensure!(
            body[..head] == DOC_SYNC_PREFIX[..head] && head == DOC_SYNC_PREFIX.len(),
            "invalid control message length"
        );
        recv.read_exact(&mut body[head..]).await?;
    } else {
        recv.read_exact(&mut body).await?;
    }
    let message: ControlMessage = serde_json::from_slice(&body)?;
    tracing::debug!(
        target: "dashbeam::_events::control::msg_in",
        kind = message.kind(),
        bytes = len,
    );
    Ok(message)
}

#[cfg(test)]
mod nearby_message_tests {
    use super::{read_message, write_message, ControlMessage};

    #[tokio::test]
    async fn who_are_you_round_trips() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        write_message(&mut client, &ControlMessage::WhoAreYou)
            .await
            .unwrap();
        let got = read_message(&mut server).await.unwrap();
        assert!(matches!(got, ControlMessage::WhoAreYou));
    }

    #[tokio::test]
    async fn identity_round_trips_with_all_fields() {
        let (mut client, mut server) = tokio::io::duplex(4096);
        let sent = ControlMessage::Identity {
            endpoint_id: "aa".repeat(32),
            display_name: "Tony's MacBook".to_string(),
            device_type: "laptop".to_string(),
            os: "macos".to_string(),
        };
        write_message(&mut client, &sent).await.unwrap();
        match read_message(&mut server).await.unwrap() {
            ControlMessage::Identity {
                endpoint_id,
                display_name,
                device_type,
                os,
            } => {
                assert_eq!(endpoint_id, "aa".repeat(32));
                assert_eq!(display_name, "Tony's MacBook");
                assert_eq!(device_type, "laptop");
                assert_eq!(os, "macos");
            }
            other => panic!("expected Identity, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn large_doc_sync_round_trips_but_other_large_messages_do_not() {
        let big = "A".repeat(3 * 1024 * 1024);
        let (mut client, mut server) = tokio::io::duplex(8 * 1024 * 1024);
        let sync = ControlMessage::DocSync { data: big.clone() };
        write_message(&mut client, &sync).await.unwrap();
        match read_message(&mut server).await.unwrap() {
            ControlMessage::DocSync { data } => assert_eq!(data, big),
            other => panic!("expected DocSync, got {other:?}"),
        }

        let other = ControlMessage::Identity {
            endpoint_id: big,
            display_name: String::new(),
            device_type: String::new(),
            os: String::new(),
        };
        assert!(write_message(&mut client, &other).await.is_err());

        // A peer that skips the writer's check is refused on read.
        let body = serde_json::to_vec(&other).unwrap();
        use tokio::io::AsyncWriteExt;
        client
            .write_all(&(body.len() as u32).to_be_bytes())
            .await
            .unwrap();
        client.write_all(&body).await.unwrap();
        assert!(read_message(&mut server).await.is_err());
    }

    #[test]
    fn tags_are_kebab_case() {
        let who = serde_json::to_value(ControlMessage::WhoAreYou).unwrap();
        assert_eq!(who["type"], "who-are-you");

        let identity = serde_json::to_value(ControlMessage::Identity {
            endpoint_id: "bb".repeat(32),
            display_name: "n".to_string(),
            device_type: "desktop".to_string(),
            os: "linux".to_string(),
        })
        .unwrap();
        assert_eq!(identity["type"], "identity");
    }

    /// `kind()` feeds the diagnostic logs, so it must not drift from the wire
    /// tag, a renamed variant would otherwise silently relabel every log line.
    #[test]
    fn kind_matches_the_serde_tag() {
        let samples = [
            ControlMessage::PairingInfo {
                endpoint_id: String::new(),
                display_name: String::new(),
                device_type: String::new(),
                os: String::new(),
                signature: String::new(),
            },
            ControlMessage::RememberVote {
                session_id: String::new(),
                vote: super::RememberVote::Remember,
            },
            ControlMessage::Invite {
                blob_ticket: String::new(),
                file_count: 0,
                total_size: 0,
                sender_name: String::new(),
            },
            ControlMessage::InviteResponse {
                session_id: String::new(),
                response: super::InviteResponse::Accepted,
            },
            ControlMessage::Recognition {
                signature: String::new(),
            },
            ControlMessage::Forget {
                signature: String::new(),
            },
            ControlMessage::WhoAreYou,
            ControlMessage::Identity {
                endpoint_id: String::new(),
                display_name: String::new(),
                device_type: String::new(),
                os: String::new(),
            },
            ControlMessage::PairRequest {
                sender_name: String::new(),
                device_type: String::new(),
                os: String::new(),
            },
            ControlMessage::DocSync {
                data: String::new(),
            },
        ];

        for message in samples {
            let kind = message.kind();
            let tag = serde_json::to_value(&message).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string();
            assert_eq!(kind, tag, "kind() drifted from the serde tag");
        }
    }

    #[test]
    fn identity_os_defaults_when_absent() {
        let json = serde_json::json!({
            "type": "identity",
            "endpoint_id": "cc".repeat(32),
            "display_name": "n",
            "device_type": "phone",
        });
        match serde_json::from_value::<ControlMessage>(json).unwrap() {
            ControlMessage::Identity { os, .. } => assert_eq!(os, ""),
            other => panic!("expected Identity, got {other:?}"),
        }
    }
}
