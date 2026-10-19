use qrcode::QrCode;
use qrcode::types::Color;
use std::fmt;

pub const PAIRING_QR_SCHEME: &str = "weavelo://join";
pub const QR_QUIET_ZONE_MODULES: usize = 4;
const MAX_PAIRING_PAYLOAD_BYTES: usize = 4096;

#[derive(Clone, PartialEq, Eq)]
pub struct PairingQrPayload {
    pub invite_code: String,
    pub control_port: u16,
}

impl fmt::Debug for PairingQrPayload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingQrPayload")
            .field("invite_code", &"<redacted>")
            .field("control_port", &self.control_port)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrMatrix {
    pub width: usize,
    pub modules: Vec<bool>,
}

impl QrMatrix {
    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        self.modules
            .get(y * self.width + x)
            .copied()
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingQrError {
    EmptyInvite,
    InvalidQr,
    Encode,
}

impl fmt::Display for PairingQrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PairingQrError::EmptyInvite => write!(f, "pairing invite is empty"),
            PairingQrError::InvalidQr => write!(f, "pairing QR payload is invalid"),
            PairingQrError::Encode => write!(f, "failed to encode pairing QR"),
        }
    }
}

impl std::error::Error for PairingQrError {}

pub fn encode_pairing_qr(invite_code: &str, control_port: u16) -> Result<String, PairingQrError> {
    let invite = invite_code.trim();
    if invite.is_empty() {
        return Err(PairingQrError::EmptyInvite);
    }
    if control_port == 0 {
        return Err(PairingQrError::InvalidQr);
    }
    // Canonical RPM2 uses URI-safe characters. Reuse the invitation validator,
    // including its checksum, instead of introducing another credential format.
    let invite = crate::mesh::MeshInvite::decode(invite)
        .map_err(|_| PairingQrError::InvalidQr)?
        .encode_copy_code();
    Ok(format!(
        "{PAIRING_QR_SCHEME}?port={control_port}&code={invite}"
    ))
}

pub fn parse_pairing_qr(raw: &str) -> Result<PairingQrPayload, PairingQrError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(PairingQrError::EmptyInvite);
    }
    if raw.len() > MAX_PAIRING_PAYLOAD_BYTES {
        return Err(PairingQrError::InvalidQr);
    }
    if let Some(rest) = raw.strip_prefix(PAIRING_QR_SCHEME) {
        // Keep the previous /CODE form readable, but never accept lookalike
        // routes such as weavelo://join-other as the join endpoint.
        let rest = rest
            .strip_prefix('?')
            .or_else(|| rest.strip_prefix('/'))
            .ok_or(PairingQrError::InvalidQr)?;
        let mut invite_code = None;
        let mut control_port = crate::net::DEFAULT_CONTROL_PORT;
        let mut has_port = false;
        if rest.contains('=') {
            for part in rest.split('&') {
                if let Some(code) = part.strip_prefix("code=") {
                    if invite_code.is_some() || code.trim().is_empty() {
                        return Err(PairingQrError::InvalidQr);
                    }
                    invite_code = Some(code.to_string());
                } else if let Some(port) = part.strip_prefix("port=") {
                    if has_port {
                        return Err(PairingQrError::InvalidQr);
                    }
                    control_port = port
                        .parse::<u16>()
                        .ok()
                        .filter(|port| *port != 0)
                        .ok_or(PairingQrError::InvalidQr)?;
                    has_port = true;
                }
            }
        } else if !rest.is_empty() {
            invite_code = Some(rest.to_string());
        }
        return Ok(PairingQrPayload {
            invite_code: invite_code.ok_or(PairingQrError::InvalidQr)?,
            control_port,
        });
    }
    if raw.contains("://") {
        return Err(PairingQrError::InvalidQr);
    }
    Ok(PairingQrPayload {
        invite_code: raw.to_string(),
        control_port: crate::net::DEFAULT_CONTROL_PORT,
    })
}

pub fn qr_matrix_from_payload(payload: &str) -> Result<QrMatrix, PairingQrError> {
    let code = QrCode::new(payload.as_bytes()).map_err(|_| PairingQrError::Encode)?;
    let width = code.width();
    let modules = code
        .to_colors()
        .into_iter()
        .map(|color| color == Color::Dark)
        .collect();
    Ok(QrMatrix { width, modules })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_qr_roundtrips_invite_and_port() {
        let code = crate::mesh::MeshConfig::generate("Synthetic").invite_code();
        let encoded = encode_pairing_qr(&code, 39271).unwrap();
        assert!(encoded.starts_with(PAIRING_QR_SCHEME));
        let parsed = parse_pairing_qr(&encoded).unwrap();
        assert_eq!(parsed.invite_code, code);
        assert_eq!(parsed.control_port, 39271);
    }

    #[test]
    fn bare_invite_still_parses() {
        let code = crate::mesh::MeshConfig::generate("Synthetic").invite_code();
        let parsed = parse_pairing_qr(&code).unwrap();
        assert_eq!(parsed.invite_code, code);
    }

    #[test]
    fn pairing_qr_roundtrips_generated_mesh_invite() {
        let config = crate::mesh::MeshConfig::generate("Phone");
        let encoded = encode_pairing_qr(&config.invite_code(), 39271).unwrap();
        let parsed = parse_pairing_qr(&encoded).unwrap();
        let joined =
            crate::mesh::MeshConfig::from_invite_code(&parsed.invite_code, "Android").unwrap();
        assert_eq!(joined.network_name, config.network_name);
        assert_eq!(joined.network_secret, config.network_secret);
        assert_ne!(joined.node_id, config.node_id);
        assert_eq!(parsed.control_port, 39271);
    }

    #[test]
    fn qr_matrix_is_square_and_nonempty() {
        let code = crate::mesh::MeshConfig::generate("Synthetic").invite_code();
        let payload = encode_pairing_qr(&code, 39271).unwrap();
        let matrix = qr_matrix_from_payload(&payload).unwrap();
        assert!(matrix.width >= 21);
        assert_eq!(matrix.modules.len(), matrix.width * matrix.width);
        assert!(matrix.modules.iter().any(|dark| *dark));
    }

    #[test]
    fn malformed_wrappers_fail_without_echoing_credentials() {
        for raw in [
            "weavelo://join",
            "weavelo://join?port=5154",
            "weavelo://join?code=",
            "weavelo://join?code=one&code=two",
            "weavelo://join?code=one&port=0",
            "weavelo://join?code=one&port=65536",
            "weavelo://join?code=one&port=abc",
            "weavelo://join?code=one&port=1&port=2",
            "weavelo://join-other?code=one",
            "https://example.invalid/?code=one",
        ] {
            assert_eq!(
                parse_pairing_qr(raw).unwrap_err(),
                PairingQrError::InvalidQr
            );
        }
        assert_eq!(
            parse_pairing_qr(&"x".repeat(4097)).unwrap_err(),
            PairingQrError::InvalidQr
        );
        assert_eq!(
            encode_pairing_qr("invalid", 5154).unwrap_err(),
            PairingQrError::InvalidQr
        );
        let payload = parse_pairing_qr("synthetic-secret").unwrap();
        assert!(!format!("{payload:?}").contains("synthetic-secret"));
    }

    #[test]
    fn legacy_path_and_manual_grouping_keep_existing_invite_semantics() {
        let code = crate::mesh::MeshConfig::generate("Synthetic").invite_code();
        let noisy = code.to_lowercase().replace('-', " \n");
        let encoded = encode_pairing_qr(&noisy, 5154).unwrap();
        assert_eq!(parse_pairing_qr(&encoded).unwrap().invite_code, code);
        let path = format!("weavelo://join/{code}");
        assert_eq!(parse_pairing_qr(&path).unwrap().invite_code, code);
    }

    #[test]
    fn android_fixture_is_the_current_rust_qr_and_a_valid_synthetic_invitation() {
        let mut lines = include_str!("../tests/fixtures/pairing-qr.txt").lines();
        let payload = lines.next().unwrap();
        let parsed = parse_pairing_qr(payload).unwrap();
        let config =
            crate::mesh::MeshConfig::from_invite_code(&parsed.invite_code, "Fixture reader")
                .unwrap();
        assert_eq!(config.network_name, "weavelo-qr-fixture");
        assert_eq!(
            config.network_secret.expose_secret(),
            "synthetic-test-only-not-a-real-secret"
        );
        assert_eq!(
            encode_pairing_qr(&config.invite_code(), parsed.control_port).unwrap(),
            payload
        );
        let matrix = qr_matrix_from_payload(payload).unwrap();
        let rows: Vec<_> = lines.collect();
        assert_eq!(rows.len(), matrix.width);
        for (y, row) in rows.iter().enumerate() {
            let generated: String = (0..matrix.width)
                .map(|x| if matrix.is_dark(x, y) { '1' } else { '0' })
                .collect();
            assert_eq!(&generated, row);
        }
    }
}
