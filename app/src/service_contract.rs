//! Versioned local service API contract, separate from executable/process trust.
//!
//! Negotiation grants no authority. The existing private IPC, executable hash,
//! profile and generation checks must succeed before this contract is used.
//! This module does not change the network Session V2 protocol.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub mod session;

pub const VERSION: Version = Version { major: 1, minor: 1 };
pub const SNAPSHOT: &str = "snapshot.v1";
pub const PAIRING: &str = "pairing.v1";
pub const CONNECTION_DIAGNOSTICS: &str = "connection-diagnostics.v1";
pub const SHUTDOWN: &str = "shutdown.v1";
const MAX_CAPABILITIES: usize = 32;
const MAX_CAPABILITY_BYTES: usize = 96;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

/// Additive fields and unknown optional capability names are allowed within a
/// major. Capabilities retain their semantics throughout that major; a semantic
/// change needs a new capability name or major, not a package-version check.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Selection {
    pub version: Version,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Rejection {
    InvalidOffer,
    MajorMismatch,
    MissingCapability(String),
}

pub fn current_offer() -> Offer {
    let mut capabilities = [SNAPSHOT, PAIRING, CONNECTION_DIAGNOSTICS].map(str::to_owned).to_vec();
    // Only the installed macOS/Linux service policy has a reviewed stop path.
    // Windows standalone Quit still cleans its own embedded host directly.
    if cfg!(any(target_os = "macos", target_os = "linux")) { capabilities.push(SHUTDOWN.into()); }
    Offer {
        version: VERSION,
        capabilities,
        required: vec![SNAPSHOT.into()],
    }
}

impl Offer {
    fn validate(&self) -> Result<BTreeSet<&str>, Rejection> {
        fn names(values: &[String]) -> Result<BTreeSet<&str>, Rejection> {
            if values.len() > MAX_CAPABILITIES {
                return Err(Rejection::InvalidOffer);
            }
            let mut names = BTreeSet::new();
            for name in values {
                if name.is_empty()
                    || name.len() > MAX_CAPABILITY_BYTES
                    || !name
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".-".contains(&c))
                    || !names.insert(name.as_str())
                {
                    return Err(Rejection::InvalidOffer);
                }
            }
            Ok(names)
        }
        let capabilities = names(&self.capabilities)?;
        let required = names(&self.required)?;
        if self.version.major == 0 || !required.is_subset(&capabilities) {
            return Err(Rejection::InvalidOffer);
        }
        Ok(capabilities)
    }

    /// Pure compatibility only. Callers must independently verify trust and
    /// bind the result to the currently verified service generation.
    pub fn negotiate(&self, peer: &Self) -> Result<Selection, Rejection> {
        let local = self.validate()?;
        let remote = peer.validate()?;
        if self.version.major != peer.version.major {
            return Err(Rejection::MajorMismatch);
        }
        let common: BTreeSet<_> = local.intersection(&remote).copied().collect();
        for required in self.required.iter().chain(&peer.required) {
            if !common.contains(required.as_str()) {
                return Err(Rejection::MissingCapability(required.clone()));
            }
        }
        Ok(Selection {
            version: Version {
                major: self.version.major,
                minor: self.version.minor.min(peer.version.minor),
            },
            capabilities: common.into_iter().map(str::to_owned).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_major_negotiates_lower_minor_and_ignores_unknown_optional_capabilities() {
        let old = current_offer();
        let mut newer = old.clone();
        newer.version.minor = 8;
        newer.capabilities.push("future-feature.v1".into());
        let selected = old.negotiate(&newer).unwrap();
        assert_eq!(selected.version, VERSION);
        assert!(
            !selected
                .capabilities
                .iter()
                .any(|v| v == "future-feature.v1")
        );
        assert_eq!(selected, newer.negotiate(&old).unwrap());
    }

    #[test]
    fn incompatible_major_and_required_features_fail_closed() {
        let local = current_offer();
        let mut peer = local.clone();
        peer.version.major += 1;
        assert_eq!(local.negotiate(&peer), Err(Rejection::MajorMismatch));
        peer.version = VERSION;
        peer.capabilities.push("session-owner.v1".into());
        peer.required.push("session-owner.v1".into());
        assert_eq!(
            local.negotiate(&peer),
            Err(Rejection::MissingCapability("session-owner.v1".into()))
        );
        peer = local.clone();
        peer.capabilities.retain(|c| c != SNAPSHOT);
        peer.required.clear();
        assert_eq!(
            local.negotiate(&peer),
            Err(Rejection::MissingCapability(SNAPSHOT.into()))
        );
    }

    #[test]
    fn capability_lists_are_bounded_unique_and_requirements_must_be_offered() {
        let local = current_offer();
        let invalid = [
            Offer {
                capabilities: vec![SNAPSHOT.into(); 33],
                ..local.clone()
            },
            Offer {
                capabilities: vec![SNAPSHOT.into(), SNAPSHOT.into()],
                ..local.clone()
            },
            Offer {
                required: vec!["not-offered.v1".into()],
                ..local.clone()
            },
            Offer {
                required: vec![SNAPSHOT.into(), SNAPSHOT.into()],
                ..local.clone()
            },
            Offer {
                version: Version { major: 0, minor: 0 },
                ..local.clone()
            },
            Offer {
                capabilities: vec!["x".repeat(97)],
                required: vec![],
                ..local.clone()
            },
            Offer {
                capabilities: vec![" ".into()],
                required: vec![],
                ..local.clone()
            },
        ];
        for offer in invalid {
            assert_eq!(local.negotiate(&offer), Err(Rejection::InvalidOffer));
            assert_eq!(offer.negotiate(&local), Err(Rejection::InvalidOffer));
        }
    }

    #[test]
    fn negotiation_json_accepts_additive_fields_without_claiming_unimplemented_apis() {
        let offer: Offer = serde_json::from_str(r#"{"version":{"major":1,"minor":3,"future":true},"capabilities":["snapshot.v1"],"future":{}}"#).unwrap();
        assert!(current_offer().negotiate(&offer).is_ok());
        let current = current_offer();
        for absent in [
            "session-owner.v1",
            "settings.v1",
            "exit-all.v1",
            "portal.v1",
            "tray.v1",
        ] {
            assert!(!current.capabilities.iter().any(|name| name == absent));
        }
        let selected = current.negotiate(&current).unwrap();
        let encoded = serde_json::to_vec(&selected).unwrap();
        assert_eq!(
            serde_json::from_slice::<Selection>(&encoded).unwrap(),
            selected
        );
    }
}
