use qrcode::QrCode;
use qrcode::types::Color;
use std::fmt;

pub const PAIRING_QR_SCHEME: &str = "remoteplay://join";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingQrPayload {
    pub invite_code: String,
    pub control_port: u16,
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
    Ok(format!(
        "{PAIRING_QR_SCHEME}?port={control_port}&code={invite}"
    ))
}

pub fn parse_pairing_qr(raw: &str) -> Result<PairingQrPayload, PairingQrError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(PairingQrError::EmptyInvite);
    }
    if let Some(rest) = raw.strip_prefix(PAIRING_QR_SCHEME) {
        let rest = rest.trim_start_matches('?').trim_start_matches('/');
        let mut invite_code = String::new();
        let mut control_port = crate::net::DEFAULT_CONTROL_PORT;
        if rest.contains('=') {
            for part in rest.split('&') {
                if let Some(code) = part.strip_prefix("code=") {
                    invite_code = code.to_string();
                } else if let Some(port) = part.strip_prefix("port=")
                    && let Ok(parsed) = port.parse::<u16>()
                {
                    control_port = parsed;
                }
            }
        } else if !rest.is_empty() {
            invite_code = rest.to_string();
        }
        if invite_code.is_empty() {
            return Err(PairingQrError::InvalidQr);
        }
        return Ok(PairingQrPayload {
            invite_code,
            control_port,
        });
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
        let encoded = encode_pairing_qr("RP1-ABCDEF", 39271).unwrap();
        assert!(encoded.starts_with(PAIRING_QR_SCHEME));
        let parsed = parse_pairing_qr(&encoded).unwrap();
        assert_eq!(parsed.invite_code, "RP1-ABCDEF");
        assert_eq!(parsed.control_port, 39271);
    }

    #[test]
    fn bare_invite_still_parses() {
        let parsed = parse_pairing_qr("RP1-XYZ").unwrap();
        assert_eq!(parsed.invite_code, "RP1-XYZ");
    }

    #[test]
    fn pairing_qr_roundtrips_generated_mesh_invite() {
        let config = crate::mesh::MeshConfig::generate("Phone");
        let encoded = encode_pairing_qr(&config.invite_code(), 39271).unwrap();
        let parsed = parse_pairing_qr(&encoded).unwrap();
        let joined =
            crate::mesh::MeshConfig::from_invite_code(&parsed.invite_code, "Android").unwrap();
        assert_eq!(joined.network_name, config.network_name);
        assert_eq!(parsed.control_port, 39271);
    }

    #[test]
    fn qr_matrix_is_square_and_nonempty() {
        let payload = encode_pairing_qr("RP1-ABCDEF", 39271).unwrap();
        let matrix = qr_matrix_from_payload(&payload).unwrap();
        assert!(matrix.width >= 21);
        assert_eq!(matrix.modules.len(), matrix.width * matrix.width);
        assert!(matrix.modules.iter().any(|dark| *dark));
    }
}
