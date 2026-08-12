use rand::random;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::process::Stdio;
use std::time::Duration;
use tokio::sync::{broadcast, watch};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub const DEFAULT_EASYTIER_PUBLIC_PEER: &str = "tcp://public.easytier.cn:11010";
const LEGACY_EASYTIER_PUBLIC_PEER: &str = "tcp://public.easytier.top:11010";
#[cfg(windows)]
pub const DEFAULT_EASYTIER_BINARY_NAME: &str = "easytier-core.exe";
#[cfg(not(windows))]
pub const DEFAULT_EASYTIER_BINARY_NAME: &str = "easytier-core";
#[cfg(windows)]
pub const DEFAULT_EASYTIER_CLI_BINARY_NAME: &str = "easytier-cli.exe";
#[cfg(not(windows))]
pub const DEFAULT_EASYTIER_CLI_BINARY_NAME: &str = "easytier-cli";
pub const MESH_CONFIG_FILE_NAME: &str = "mesh.conf";
pub const MESH_SECRET_FILE_NAME: &str = "mesh.secret";
pub const REMOTE_PLAY_EASYTIER_BIN_ENV: &str = "REMOTE_PLAY_EASYTIER_BIN";
pub const REMOTE_PLAY_EASYTIER_CLI_BIN_ENV: &str = "REMOTE_PLAY_EASYTIER_CLI_BIN";
pub const REMOTE_PLAY_MESH_DIR_ENV: &str = "REMOTE_PLAY_MESH_DIR";
pub const REMOTE_PLAY_MESH_ENV: &str = "REMOTE_PLAY_MESH";
pub const REMOTE_PLAY_MESH_COPY_CODE_PREFIX: &str = "RPM2";
pub const REMOTE_PLAY_MESH_INVITE_PREFIX: &str = "rpmesh1";
pub const EASYTIER_SIDECAR_LOG_FILE_NAME: &str = "easytier-sidecar.log";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshIntegrationMode {
    BundledEasyTierSidecar,
}

#[derive(Clone, PartialEq, Eq)]
pub struct MeshSecret(String);

impl MeshSecret {
    pub fn new(value: impl Into<String>) -> Result<Self, MeshConfigError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(MeshConfigError::EmptyNetworkSecret);
        }
        Ok(Self(value))
    }

    pub fn generate() -> Self {
        Self(hex_random::<32>())
    }

    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MeshSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MeshSecret(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPeerEndpoint(String);

impl MeshPeerEndpoint {
    pub fn new(value: impl Into<String>) -> Result<Self, MeshConfigError> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() {
            return Err(MeshConfigError::EmptyPeerEndpoint);
        }
        if value.contains('|') {
            return Err(MeshConfigError::InvalidPeerEndpoint(value.to_string()));
        }
        Ok(Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshConfig {
    pub network_name: String,
    pub network_secret: MeshSecret,
    pub node_id: String,
    pub display_name: String,
    pub initial_peers: Vec<MeshPeerEndpoint>,
    pub auto_start: bool,
    pub integration_mode: MeshIntegrationMode,
}

impl MeshConfig {
    pub fn generate(display_name: impl Into<String>) -> Self {
        let display_name = sanitized_display_name(display_name);
        Self {
            network_name: format!("remote-play-{}", hex_random::<8>()),
            network_secret: MeshSecret::generate(),
            node_id: hex_random::<16>(),
            display_name,
            initial_peers: vec![
                MeshPeerEndpoint::new(DEFAULT_EASYTIER_PUBLIC_PEER)
                    .expect("default EasyTier peer must be valid"),
            ],
            auto_start: true,
            integration_mode: MeshIntegrationMode::BundledEasyTierSidecar,
        }
    }

    pub fn from_invite_code(
        invite_code: &str,
        display_name: impl Into<String>,
    ) -> Result<Self, MeshConfigError> {
        let invite = MeshInvite::decode(invite_code)?;
        let mut config = Self::generate(display_name);
        config.network_name = invite.network_name;
        config.network_secret = invite.network_secret;
        config.initial_peers = invite.initial_peers;
        Ok(config)
    }

    pub fn invite_code(&self) -> String {
        MeshInvite {
            network_name: self.network_name.clone(),
            network_secret: self.network_secret.clone(),
            initial_peers: self.initial_peers.clone(),
        }
        .encode_copy_code()
    }

    pub fn legacy_invite_code(&self) -> String {
        MeshInvite {
            network_name: self.network_name.clone(),
            network_secret: self.network_secret.clone(),
            initial_peers: self.initial_peers.clone(),
        }
        .encode_legacy()
    }

    pub fn validate(&self) -> Result<(), MeshConfigError> {
        if self.network_name.trim().is_empty() {
            return Err(MeshConfigError::EmptyNetworkName);
        }
        if self.display_name.trim().is_empty() {
            return Err(MeshConfigError::EmptyDisplayName);
        }
        if self.node_id.trim().is_empty() {
            return Err(MeshConfigError::EmptyNodeId);
        }
        if self.network_secret.expose_secret().trim().is_empty() {
            return Err(MeshConfigError::EmptyNetworkSecret);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshInvite {
    pub network_name: String,
    pub network_secret: MeshSecret,
    pub initial_peers: Vec<MeshPeerEndpoint>,
}

impl MeshInvite {
    pub fn encode(&self) -> String {
        self.encode_copy_code()
    }

    pub fn encode_legacy(&self) -> String {
        let peers = self
            .initial_peers
            .iter()
            .map(MeshPeerEndpoint::as_str)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{}|{}|{}|{}",
            REMOTE_PLAY_MESH_INVITE_PREFIX,
            self.network_name,
            self.network_secret.expose_secret(),
            peers
        )
    }

    pub fn decode(value: &str) -> Result<Self, MeshConfigError> {
        if looks_like_copy_code(value) {
            return decode_mesh_copy_code(value);
        }
        Self::decode_legacy(value)
    }

    pub fn encode_copy_code(&self) -> String {
        let payload = encode_mesh_copy_code_payload(self);
        let encoded = crockford_base32_encode(&payload);
        format!(
            "{REMOTE_PLAY_MESH_COPY_CODE_PREFIX}-{}",
            group_copy_code(&encoded)
        )
    }

    fn decode_legacy(value: &str) -> Result<Self, MeshConfigError> {
        let parts = value.split('|').collect::<Vec<_>>();
        if parts.len() != 4 || parts[0] != REMOTE_PLAY_MESH_INVITE_PREFIX {
            return Err(MeshConfigError::InvalidInviteCode);
        }
        if parts[1].trim().is_empty() {
            return Err(MeshConfigError::EmptyNetworkName);
        }
        let peers = if parts[3].trim().is_empty() {
            Vec::new()
        } else {
            parts[3]
                .split(',')
                .map(MeshPeerEndpoint::new)
                .collect::<Result<Vec<_>, _>>()?
        };
        Ok(Self {
            network_name: parts[1].to_string(),
            network_secret: MeshSecret::new(parts[2])?,
            initial_peers: peers,
        })
    }
}

const MESH_COPY_CODE_VERSION: u8 = 1;
const MESH_COPY_CODE_FLAG_DEFAULT_PUBLIC_PEER: u8 = 0x01;
const MESH_COPY_SECRET_UTF8: u8 = 0;
const MESH_COPY_SECRET_HEX_BYTES: u8 = 1;

fn looks_like_copy_code(value: &str) -> bool {
    let compact = compact_copy_code(value);
    compact
        .get(..REMOTE_PLAY_MESH_COPY_CODE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(REMOTE_PLAY_MESH_COPY_CODE_PREFIX))
}

fn encode_mesh_copy_code_payload(invite: &MeshInvite) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(MESH_COPY_CODE_VERSION);
    let default_peers = is_default_public_peer_list(&invite.initial_peers);
    payload.push(if default_peers {
        MESH_COPY_CODE_FLAG_DEFAULT_PUBLIC_PEER
    } else {
        0
    });

    push_u8_len_bytes(&mut payload, invite.network_name.as_bytes());
    let (secret_kind, secret_bytes) =
        encode_copy_code_secret(invite.network_secret.expose_secret());
    payload.push(secret_kind);
    push_u8_len_bytes(&mut payload, &secret_bytes);

    if default_peers {
        payload.push(0);
    } else {
        payload.push(invite.initial_peers.len().try_into().unwrap_or(u8::MAX));
        for peer in invite.initial_peers.iter().take(u8::MAX as usize) {
            push_u16_len_bytes(&mut payload, peer.as_str().as_bytes());
        }
    }

    let checksum = fnv1a32(&payload);
    payload.extend_from_slice(&checksum.to_be_bytes());
    payload
}

fn decode_mesh_copy_code(value: &str) -> Result<MeshInvite, MeshConfigError> {
    let compact = compact_copy_code(value);
    if !compact
        .get(..REMOTE_PLAY_MESH_COPY_CODE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(REMOTE_PLAY_MESH_COPY_CODE_PREFIX))
    {
        return Err(MeshConfigError::InvalidInviteCode);
    }
    let encoded = &compact[REMOTE_PLAY_MESH_COPY_CODE_PREFIX.len()..];
    let payload = crockford_base32_decode(encoded)?;
    if payload.len() < 5 {
        return Err(MeshConfigError::InvalidInviteCode);
    }

    let content_len = payload.len() - 4;
    let (content, checksum_bytes) = payload.split_at(content_len);
    let expected = u32::from_be_bytes(
        checksum_bytes
            .try_into()
            .map_err(|_| MeshConfigError::InvalidInviteCode)?,
    );
    if fnv1a32(content) != expected {
        return Err(MeshConfigError::InvalidInviteCode);
    }

    let mut reader = MeshCopyCodeReader::new(content);
    let version = reader.read_u8()?;
    if version != MESH_COPY_CODE_VERSION {
        return Err(MeshConfigError::InvalidInviteCode);
    }
    let flags = reader.read_u8()?;
    let network_name = String::from_utf8(reader.read_u8_len_bytes()?.to_vec())
        .map_err(|_| MeshConfigError::InvalidInviteCode)?;
    if network_name.trim().is_empty() {
        return Err(MeshConfigError::EmptyNetworkName);
    }

    let secret_kind = reader.read_u8()?;
    let secret_bytes = reader.read_u8_len_bytes()?;
    let secret = decode_copy_code_secret(secret_kind, secret_bytes)?;
    let peer_count = reader.read_u8()?;
    let initial_peers = if flags & MESH_COPY_CODE_FLAG_DEFAULT_PUBLIC_PEER != 0 {
        if peer_count != 0 {
            return Err(MeshConfigError::InvalidInviteCode);
        }
        vec![MeshPeerEndpoint::new(DEFAULT_EASYTIER_PUBLIC_PEER)?]
    } else {
        let mut peers = Vec::with_capacity(peer_count as usize);
        for _ in 0..peer_count {
            let peer = String::from_utf8(reader.read_u16_len_bytes()?.to_vec())
                .map_err(|_| MeshConfigError::InvalidInviteCode)?;
            peers.push(MeshPeerEndpoint::new(peer)?);
        }
        peers
    };
    if !reader.is_finished() {
        return Err(MeshConfigError::InvalidInviteCode);
    }

    Ok(MeshInvite {
        network_name,
        network_secret: MeshSecret::new(secret)?,
        initial_peers,
    })
}

fn encode_copy_code_secret(secret: &str) -> (u8, Vec<u8>) {
    if let Some(bytes) = decode_hex_bytes_for_copy_code(secret) {
        (MESH_COPY_SECRET_HEX_BYTES, bytes)
    } else {
        (MESH_COPY_SECRET_UTF8, secret.as_bytes().to_vec())
    }
}

fn decode_copy_code_secret(secret_kind: u8, bytes: &[u8]) -> Result<String, MeshConfigError> {
    match secret_kind {
        MESH_COPY_SECRET_UTF8 => {
            String::from_utf8(bytes.to_vec()).map_err(|_| MeshConfigError::InvalidInviteCode)
        }
        MESH_COPY_SECRET_HEX_BYTES => Ok(hex_bytes(bytes)),
        _ => Err(MeshConfigError::InvalidInviteCode),
    }
}

fn is_default_public_peer_list(peers: &[MeshPeerEndpoint]) -> bool {
    peers.len() == 1
        && matches!(
            peers[0].as_str(),
            DEFAULT_EASYTIER_PUBLIC_PEER | LEGACY_EASYTIER_PUBLIC_PEER
        )
}

fn push_u8_len_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(bytes.len().try_into().unwrap_or(u8::MAX));
    out.extend_from_slice(&bytes[..bytes.len().min(u8::MAX as usize)]);
}

fn push_u16_len_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = bytes.len().min(u16::MAX as usize);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&bytes[..len]);
}

fn compact_copy_code(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect()
}

fn group_copy_code(encoded: &str) -> String {
    const GROUP: usize = 8;
    let mut grouped = String::with_capacity(encoded.len() + encoded.len() / GROUP);
    for (idx, ch) in encoded.chars().enumerate() {
        if idx > 0 && idx % GROUP == 0 {
            grouped.push('-');
        }
        grouped.push(ch);
    }
    grouped
}

fn crockford_base32_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = String::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            let shift = bits - 5;
            let idx = ((buffer >> shift) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
            bits -= 5;
            buffer &= (1 << bits) - 1;
        }
    }
    if bits > 0 {
        let idx = ((buffer << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

fn crockford_base32_decode(value: &str) -> Result<Vec<u8>, MeshConfigError> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for ch in value.chars() {
        let Some(value) = crockford_base32_value(ch) else {
            return Err(MeshConfigError::InvalidInviteCode);
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        while bits >= 8 {
            let shift = bits - 8;
            out.push(((buffer >> shift) & 0xff) as u8);
            bits -= 8;
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn crockford_base32_value(ch: char) -> Option<u8> {
    match ch.to_ascii_uppercase() {
        '0' | 'O' => Some(0),
        '1' | 'I' | 'L' => Some(1),
        '2' => Some(2),
        '3' => Some(3),
        '4' => Some(4),
        '5' => Some(5),
        '6' => Some(6),
        '7' => Some(7),
        '8' => Some(8),
        '9' => Some(9),
        'A' => Some(10),
        'B' => Some(11),
        'C' => Some(12),
        'D' => Some(13),
        'E' => Some(14),
        'F' => Some(15),
        'G' => Some(16),
        'H' => Some(17),
        'J' => Some(18),
        'K' => Some(19),
        'M' => Some(20),
        'N' => Some(21),
        'P' => Some(22),
        'Q' => Some(23),
        'R' => Some(24),
        'S' => Some(25),
        'T' => Some(26),
        'V' => Some(27),
        'W' => Some(28),
        'X' => Some(29),
        'Y' => Some(30),
        'Z' => Some(31),
        _ => None,
    }
}

fn decode_hex_bytes_for_copy_code(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    let raw = value.as_bytes();
    for idx in (0..raw.len()).step_by(2) {
        let high = decode_hex_nibble_for_copy_code(raw[idx])?;
        let low = decode_hex_nibble_for_copy_code(raw[idx + 1])?;
        out.push((high << 4) | low);
    }
    Some(out)
}

fn decode_hex_nibble_for_copy_code(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

struct MeshCopyCodeReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> MeshCopyCodeReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, MeshConfigError> {
        let Some(value) = self.bytes.get(self.pos) else {
            return Err(MeshConfigError::InvalidInviteCode);
        };
        self.pos += 1;
        Ok(*value)
    }

    fn read_u8_len_bytes(&mut self) -> Result<&'a [u8], MeshConfigError> {
        let len = self.read_u8()? as usize;
        self.read_bytes(len)
    }

    fn read_u16_len_bytes(&mut self) -> Result<&'a [u8], MeshConfigError> {
        let high = self.read_u8()? as u16;
        let low = self.read_u8()? as u16;
        self.read_bytes(((high << 8) | low) as usize)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], MeshConfigError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(MeshConfigError::InvalidInviteCode)?;
        if end > self.bytes.len() {
            return Err(MeshConfigError::InvalidInviteCode);
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn is_finished(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshSecretStorageKind {
    AppPrivateFile,
    PlatformSecureStore,
}

pub trait MeshSecretStore {
    fn storage_kind(&self) -> MeshSecretStorageKind;
    fn load_secret(&self) -> Result<Option<MeshSecret>, MeshStoreError>;
    fn save_secret(&self, secret: &MeshSecret) -> Result<(), MeshStoreError>;
    fn delete_secret(&self) -> Result<(), MeshStoreError>;
}

pub fn default_app_private_mesh_dir() -> PathBuf {
    if let Some(path) = env::var_os(REMOTE_PLAY_MESH_DIR_ENV)
        .and_then(non_empty_env_path)
        .map(PathBuf::from)
    {
        return path;
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = env::var_os("HOME").and_then(non_empty_env_path) {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("RemotePlay")
            .join("Mesh");
    }

    #[cfg(target_os = "windows")]
    if let Some(appdata) = env::var_os("APPDATA").and_then(non_empty_env_path) {
        return PathBuf::from(appdata).join("RemotePlay").join("Mesh");
    }

    #[cfg(not(target_os = "windows"))]
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME").and_then(non_empty_env_path) {
        return PathBuf::from(config_home).join("remote-play").join("mesh");
    }

    #[cfg(not(target_os = "windows"))]
    if let Some(home) = env::var_os("HOME").and_then(non_empty_env_path) {
        return PathBuf::from(home)
            .join(".config")
            .join("remote-play")
            .join("mesh");
    }

    env::temp_dir().join("remote-play").join("mesh")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPrivateMeshSecretStore {
    path: PathBuf,
}

impl AppPrivateMeshSecretStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl MeshSecretStore for AppPrivateMeshSecretStore {
    fn storage_kind(&self) -> MeshSecretStorageKind {
        MeshSecretStorageKind::AppPrivateFile
    }

    fn load_secret(&self) -> Result<Option<MeshSecret>, MeshStoreError> {
        match fs::read_to_string(&self.path) {
            Ok(value) => {
                let value = value.trim();
                MeshSecret::new(value.to_string())
                    .map(Some)
                    .map_err(MeshStoreError::InvalidMeshConfig)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(MeshStoreError::io(self.path.clone(), source)),
        }
    }

    fn save_secret(&self, secret: &MeshSecret) -> Result<(), MeshStoreError> {
        write_private_file(&self.path, secret.expose_secret().as_bytes())
    }

    fn delete_secret(&self) -> Result<(), MeshStoreError> {
        remove_file_if_exists(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPrivateMeshConfigStore<S = AppPrivateMeshSecretStore> {
    root_dir: PathBuf,
    config_path: PathBuf,
    secret_store: S,
}

impl AppPrivateMeshConfigStore<AppPrivateMeshSecretStore> {
    pub fn new(root_dir: impl Into<PathBuf>) -> Self {
        let root_dir = root_dir.into();
        let config_path = root_dir.join(MESH_CONFIG_FILE_NAME);
        let secret_store = AppPrivateMeshSecretStore::new(root_dir.join(MESH_SECRET_FILE_NAME));
        Self {
            root_dir,
            config_path,
            secret_store,
        }
    }
}

impl<S: MeshSecretStore> AppPrivateMeshConfigStore<S> {
    pub fn with_secret_store(root_dir: impl Into<PathBuf>, secret_store: S) -> Self {
        let root_dir = root_dir.into();
        let config_path = root_dir.join(MESH_CONFIG_FILE_NAME);
        Self {
            root_dir,
            config_path,
            secret_store,
        }
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn secret_store(&self) -> &S {
        &self.secret_store
    }

    pub fn load(&self) -> Result<Option<MeshConfig>, MeshStoreError> {
        let metadata = match fs::read_to_string(&self.config_path) {
            Ok(value) => value,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(MeshStoreError::io(self.config_path.clone(), source)),
        };
        let mut config = decode_mesh_config_metadata(&metadata)?;
        let Some(secret) = self.secret_store.load_secret()? else {
            return Err(MeshStoreError::MissingNetworkSecret);
        };
        config.network_secret = secret;
        config
            .validate()
            .map_err(MeshStoreError::InvalidMeshConfig)?;
        Ok(Some(config))
    }

    pub fn load_or_generate(
        &self,
        display_name: impl Into<String>,
    ) -> Result<MeshConfig, MeshStoreError> {
        if let Some(config) = self.load()? {
            let migrated = migrate_mesh_config(&config);
            if migrated != config {
                self.save(&migrated)?;
            }
            return Ok(migrated);
        }

        let config = MeshConfig::generate(display_name);
        self.save(&config)?;
        Ok(config)
    }

    pub fn save(&self, config: &MeshConfig) -> Result<(), MeshStoreError> {
        config
            .validate()
            .map_err(MeshStoreError::InvalidMeshConfig)?;
        create_private_dir(&self.root_dir)?;
        self.secret_store.save_secret(&config.network_secret)?;
        write_private_file(
            &self.config_path,
            encode_mesh_config_metadata(config).as_bytes(),
        )
    }

    pub fn delete(&self) -> Result<(), MeshStoreError> {
        remove_file_if_exists(&self.config_path)?;
        self.secret_store.delete_secret()
    }
}

#[derive(Debug)]
pub enum MeshStoreError {
    Io { path: PathBuf, source: io::Error },
    MissingNetworkSecret,
    InvalidMetadata(String),
    InvalidMeshConfig(MeshConfigError),
}

impl MeshStoreError {
    fn io(path: PathBuf, source: io::Error) -> Self {
        Self::Io { path, source }
    }
}

impl fmt::Display for MeshStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::MissingNetworkSecret => {
                f.write_str("stored mesh config is missing network secret")
            }
            Self::InvalidMetadata(message) => write!(f, "stored mesh config is invalid: {message}"),
            Self::InvalidMeshConfig(err) => write!(f, "{err}"),
        }
    }
}

impl Error for MeshStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidMeshConfig(err) => Some(err),
            Self::MissingNetworkSecret | Self::InvalidMetadata(_) => None,
        }
    }
}

fn migrate_mesh_config(config: &MeshConfig) -> MeshConfig {
    let mut migrated = config.clone();
    for peer in &mut migrated.initial_peers {
        if peer.as_str() == LEGACY_EASYTIER_PUBLIC_PEER {
            *peer = MeshPeerEndpoint::new(DEFAULT_EASYTIER_PUBLIC_PEER)
                .expect("default EasyTier peer must be valid");
        }
    }
    migrated
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierSidecarConfig {
    pub binary_path: PathBuf,
    pub mesh: MeshConfig,
    pub virtual_ipv4: Option<Ipv4Addr>,
    pub dhcp: bool,
    pub latency_first: bool,
    pub private_mode: bool,
    pub rpc_portal: Option<String>,
    pub log_file_path: Option<PathBuf>,
}

impl EasyTierSidecarConfig {
    pub fn new(binary_path: impl Into<PathBuf>, mesh: MeshConfig) -> Self {
        let virtual_ipv4 = derive_virtual_ipv4(&mesh.network_name, &mesh.node_id);
        Self {
            binary_path: binary_path.into(),
            virtual_ipv4: Some(virtual_ipv4),
            mesh,
            dhcp: false,
            latency_first: true,
            private_mode: false,
            rpc_portal: Some("127.0.0.1:15888".to_string()),
            log_file_path: None,
        }
    }

    pub fn command_args(&self) -> Vec<String> {
        let mut args = vec![
            "--network-name".to_string(),
            self.mesh.network_name.clone(),
            "--network-secret".to_string(),
            self.mesh.network_secret.expose_secret().to_string(),
            "--hostname".to_string(),
            self.mesh.display_name.clone(),
            "--instance-name".to_string(),
            "remote-play".to_string(),
        ];

        if let Some(ipv4) = self.virtual_ipv4 {
            args.push("--ipv4".to_string());
            args.push(ipv4.to_string());
        } else if self.dhcp {
            args.push("--dhcp".to_string());
            args.push("true".to_string());
        }
        if self.latency_first {
            args.push("--latency-first".to_string());
            args.push("true".to_string());
        }
        if self.private_mode {
            args.push("--private-mode".to_string());
            args.push("true".to_string());
        }
        if let Some(rpc_portal) = &self.rpc_portal {
            args.push("--rpc-portal".to_string());
            args.push(rpc_portal.clone());
        }
        for peer in &self.mesh.initial_peers {
            args.push("-p".to_string());
            args.push(peer.as_str().to_string());
        }

        args
    }

    pub fn redacted_command_args(&self) -> Vec<String> {
        let mut args = self.command_args();
        let mut redact_next = false;
        for arg in &mut args {
            if redact_next {
                *arg = "<redacted>".to_string();
                redact_next = false;
                continue;
            }
            redact_next = arg == "--network-secret";
        }
        args
    }

    pub fn launch_plan(&self) -> EasyTierSidecarLaunchPlan {
        EasyTierSidecarLaunchPlan {
            binary_path: self.binary_path.clone(),
            args: self.command_args(),
            redacted_args: self.redacted_command_args(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct EasyTierSidecarLaunchPlan {
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub redacted_args: Vec<String>,
}

impl fmt::Debug for EasyTierSidecarLaunchPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EasyTierSidecarLaunchPlan")
            .field("binary_path", &self.binary_path)
            .field("args", &self.redacted_args)
            .finish()
    }
}

fn configure_sidecar_stdio(command: &mut tokio::process::Command, log_file_path: Option<&Path>) {
    command.stdin(Stdio::null());

    if let Some(path) = log_file_path
        && let Ok(file) = open_sidecar_log(path)
        && let Ok(stderr) = file.try_clone()
    {
        command.stdout(Stdio::from(file));
        command.stderr(Stdio::from(stderr));
        return;
    }

    command.stdout(Stdio::null());
    command.stderr(Stdio::null());
}

fn open_sidecar_log(path: &Path) -> io::Result<fs::File> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

fn read_recent_sidecar_log(path: &Path) -> Option<String> {
    const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;

    let bytes = fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(MAX_DIAGNOSTIC_BYTES);
    let diagnostic = String::from_utf8_lossy(&bytes[start..]).trim().to_string();
    if diagnostic.is_empty() {
        None
    } else {
        Some(diagnostic)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EasyTierBinarySource {
    EnvOverride,
    AppResource,
    CurrentExecutableDirectory,
    SidecarSibling,
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierBinaryCandidate {
    pub source: EasyTierBinarySource,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierBinaryLocation {
    pub source: EasyTierBinarySource,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierBinaryLocator {
    pub binary_name: String,
    pub env_override: Option<PathBuf>,
    pub app_resource_dirs: Vec<PathBuf>,
    pub current_exe_dir: Option<PathBuf>,
    pub path_dirs: Vec<PathBuf>,
}

impl EasyTierBinaryLocator {
    pub fn from_environment() -> Self {
        let env_override = env::var_os(REMOTE_PLAY_EASYTIER_BIN_ENV)
            .and_then(non_empty_env_path)
            .map(PathBuf::from);
        let current_exe = env::current_exe().ok();
        let current_exe_dir = current_exe
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let path_dirs = env::var_os("PATH")
            .map(|value| env::split_paths(&value).collect())
            .unwrap_or_default();

        Self {
            binary_name: DEFAULT_EASYTIER_BINARY_NAME.to_string(),
            env_override,
            app_resource_dirs: default_app_resource_dirs(current_exe.as_deref()),
            current_exe_dir,
            path_dirs,
        }
    }

    pub fn new(binary_name: impl Into<String>) -> Self {
        Self {
            binary_name: binary_name.into(),
            env_override: None,
            app_resource_dirs: Vec::new(),
            current_exe_dir: None,
            path_dirs: Vec::new(),
        }
    }

    pub fn with_env_override(mut self, path: impl Into<PathBuf>) -> Self {
        self.env_override = Some(path.into());
        self
    }

    pub fn with_app_resource_dirs(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.app_resource_dirs = dedup_paths(dirs);
        self
    }

    pub fn with_current_exe_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_exe_dir = Some(dir.into());
        self
    }

    pub fn with_path_dirs(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.path_dirs = dedup_paths(dirs);
        self
    }

    pub fn candidate_paths(&self) -> Vec<EasyTierBinaryCandidate> {
        let mut candidates = Vec::new();

        if let Some(path) = &self.env_override {
            candidates.push(EasyTierBinaryCandidate {
                source: EasyTierBinarySource::EnvOverride,
                path: path.clone(),
            });
        }

        for dir in &self.app_resource_dirs {
            candidates.push(EasyTierBinaryCandidate {
                source: EasyTierBinarySource::AppResource,
                path: dir.join(&self.binary_name),
            });
        }

        if let Some(dir) = &self.current_exe_dir {
            candidates.push(EasyTierBinaryCandidate {
                source: EasyTierBinarySource::CurrentExecutableDirectory,
                path: dir.join(&self.binary_name),
            });
        }

        for dir in &self.path_dirs {
            candidates.push(EasyTierBinaryCandidate {
                source: EasyTierBinarySource::Path,
                path: dir.join(&self.binary_name),
            });
        }

        dedup_candidates(candidates)
    }

    pub fn locate(&self) -> Result<EasyTierBinaryLocation, EasyTierBinaryLocateError> {
        if let Some(path) = &self.env_override {
            return validate_binary_candidate(EasyTierBinaryCandidate {
                source: EasyTierBinarySource::EnvOverride,
                path: path.clone(),
            })
            .map_err(|err| err.for_env_override());
        }

        let candidates = self
            .candidate_paths()
            .into_iter()
            .filter(|candidate| candidate.source != EasyTierBinarySource::EnvOverride)
            .collect::<Vec<_>>();
        let mut first_not_executable = None;

        for candidate in &candidates {
            match validate_binary_candidate(candidate.clone()) {
                Ok(location) => return Ok(location),
                Err(EasyTierBinaryLocateError::NotExecutable(path)) => {
                    first_not_executable.get_or_insert(path);
                }
                Err(EasyTierBinaryLocateError::NotAFile(_))
                | Err(EasyTierBinaryLocateError::NotFound { .. })
                | Err(EasyTierBinaryLocateError::EnvOverrideMissing(_)) => {}
            }
        }

        if let Some(path) = first_not_executable {
            return Err(EasyTierBinaryLocateError::NotExecutable(path));
        }

        Err(EasyTierBinaryLocateError::NotFound {
            binary_name: self.binary_name.clone(),
            searched: candidates
                .into_iter()
                .map(|candidate| candidate.path)
                .collect(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EasyTierBinaryLocateError {
    EnvOverrideMissing(PathBuf),
    NotAFile(PathBuf),
    NotExecutable(PathBuf),
    NotFound {
        binary_name: String,
        searched: Vec<PathBuf>,
    },
}

impl EasyTierBinaryLocateError {
    fn for_env_override(self) -> Self {
        match self {
            Self::NotFound { searched, .. } => {
                Self::EnvOverrideMissing(searched.into_iter().next().unwrap_or_default())
            }
            err => err,
        }
    }
}

impl fmt::Display for EasyTierBinaryLocateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvOverrideMissing(path) => {
                write!(
                    f,
                    "{REMOTE_PLAY_EASYTIER_BIN_ENV} points to a missing EasyTier binary: {}",
                    path.display()
                )
            }
            Self::NotAFile(path) => {
                write!(f, "EasyTier binary path is not a file: {}", path.display())
            }
            Self::NotExecutable(path) => {
                write!(f, "EasyTier binary is not executable: {}", path.display())
            }
            Self::NotFound {
                binary_name,
                searched,
            } => {
                write!(
                    f,
                    "EasyTier binary {binary_name} was not found in {} searched location(s)",
                    searched.len()
                )
            }
        }
    }
}

impl Error for EasyTierBinaryLocateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EasyTierProcessState {
    NotStarted,
    Running,
    Exited(Option<i32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EasyTierHealthState {
    Starting,
    Ready,
    Degraded,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EasyTierHealthIssue {
    RequiresAdminPrivileges,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierHealthSnapshot {
    pub state: EasyTierHealthState,
    pub process_state: EasyTierProcessState,
    pub virtual_ip: Option<IpAddr>,
    pub issue: Option<EasyTierHealthIssue>,
    pub message: String,
}

impl EasyTierHealthSnapshot {
    pub fn degraded(
        process_state: EasyTierProcessState,
        virtual_ip: Option<IpAddr>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            state: EasyTierHealthState::Degraded,
            process_state,
            virtual_ip,
            issue: None,
            message: message.into(),
        }
    }

    pub fn degraded_with_issue(
        process_state: EasyTierProcessState,
        virtual_ip: Option<IpAddr>,
        issue: EasyTierHealthIssue,
        message: impl Into<String>,
    ) -> Self {
        Self {
            state: EasyTierHealthState::Degraded,
            process_state,
            virtual_ip,
            issue: Some(issue),
            message: message.into(),
        }
    }

    pub fn from_process_and_probe(
        process_state: EasyTierProcessState,
        virtual_ip: Option<IpAddr>,
    ) -> Self {
        Self::from_process_probe_and_diagnostic(process_state, virtual_ip, None)
    }

    fn from_process_probe_and_diagnostic(
        process_state: EasyTierProcessState,
        virtual_ip: Option<IpAddr>,
        diagnostic: Option<&str>,
    ) -> Self {
        match process_state {
            EasyTierProcessState::Running if virtual_ip.is_some() => Self {
                state: EasyTierHealthState::Ready,
                process_state,
                virtual_ip,
                issue: None,
                message: "EasyTier sidecar is running with a virtual IP".to_string(),
            },
            EasyTierProcessState::Running => Self {
                state: EasyTierHealthState::Starting,
                process_state,
                virtual_ip,
                issue: None,
                message: "EasyTier sidecar is running; virtual IP is not known yet".to_string(),
            },
            EasyTierProcessState::NotStarted => Self {
                state: EasyTierHealthState::Stopped,
                process_state,
                virtual_ip,
                issue: None,
                message: "EasyTier sidecar is not started".to_string(),
            },
            EasyTierProcessState::Exited(code) => {
                if let Some(issue) = diagnostic.and_then(classify_easytier_health_issue) {
                    return Self::degraded_with_issue(
                        process_state,
                        virtual_ip,
                        issue,
                        easytier_health_issue_message(issue),
                    );
                }

                Self {
                    state: EasyTierHealthState::Degraded,
                    process_state,
                    virtual_ip,
                    issue: None,
                    message: match code {
                        Some(code) => format!("EasyTier sidecar exited with status {code}"),
                        None => "EasyTier sidecar exited without a status code".to_string(),
                    },
                }
            }
        }
    }
}

fn classify_easytier_health_issue(diagnostic: &str) -> Option<EasyTierHealthIssue> {
    let lower = diagnostic.to_ascii_lowercase();
    let mentions_tun = lower.contains("tun")
        || lower.contains("utun")
        || lower.contains("virtual network")
        || lower.contains("network adapter");
    let mentions_permission = lower.contains("operation not permitted")
        || lower.contains("permission denied")
        || lower.contains("requires administrator")
        || lower.contains("requires root");

    if mentions_tun && mentions_permission {
        return Some(EasyTierHealthIssue::RequiresAdminPrivileges);
    }

    None
}

fn easytier_health_issue_message(issue: EasyTierHealthIssue) -> &'static str {
    match issue {
        EasyTierHealthIssue::RequiresAdminPrivileges => {
            "EasyTier needs administrator permission to create the virtual network adapter."
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierCliProbeConfig {
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl EasyTierCliProbeConfig {
    pub fn node(binary_path: impl Into<PathBuf>) -> Self {
        Self {
            binary_path: binary_path.into(),
            args: vec!["node".to_string()],
            timeout: Duration::from_secs(2),
        }
    }

    pub fn from_sidecar_binary(sidecar_binary_path: &Path) -> Self {
        if let Some(path) = env::var_os(REMOTE_PLAY_EASYTIER_CLI_BIN_ENV)
            .and_then(non_empty_env_path)
            .map(PathBuf::from)
        {
            return Self::node(path);
        }

        let path = sidecar_binary_path
            .parent()
            .map(|dir| dir.join(DEFAULT_EASYTIER_CLI_BINARY_NAME))
            .unwrap_or_else(|| PathBuf::from(DEFAULT_EASYTIER_CLI_BINARY_NAME));
        Self::node(path)
    }

    pub fn redacted_args(&self) -> Vec<String> {
        self.args.clone()
    }

    pub async fn run(&self) -> Result<EasyTierCliProbeResult, EasyTierProbeError> {
        let mut command = tokio::process::Command::new(&self.binary_path);
        command
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = tokio::time::timeout(self.timeout, command.output())
            .await
            .map_err(|_| EasyTierProbeError::TimedOut {
                binary_path: self.binary_path.clone(),
                timeout: self.timeout,
            })?
            .map_err(|source| EasyTierProbeError::SpawnFailed {
                binary_path: self.binary_path.clone(),
                source,
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(EasyTierProbeError::ExitFailed {
                binary_path: self.binary_path.clone(),
                status: output.status,
                stderr,
            });
        }

        Ok(EasyTierCliProbeResult {
            virtual_ip: parse_virtual_ip_from_probe_text(&stdout),
            stdout,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierCliProbeResult {
    pub virtual_ip: Option<IpAddr>,
    pub stdout: String,
}

#[derive(Debug)]
pub enum EasyTierProbeError {
    SpawnFailed {
        binary_path: PathBuf,
        source: io::Error,
    },
    TimedOut {
        binary_path: PathBuf,
        timeout: Duration,
    },
    ExitFailed {
        binary_path: PathBuf,
        status: ExitStatus,
        stderr: String,
    },
}

impl fmt::Display for EasyTierProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpawnFailed {
                binary_path,
                source,
            } => write!(
                f,
                "failed to run EasyTier probe {}: {source}",
                binary_path.display()
            ),
            Self::TimedOut {
                binary_path,
                timeout,
            } => write!(
                f,
                "EasyTier probe {} timed out after {} ms",
                binary_path.display(),
                timeout.as_millis()
            ),
            Self::ExitFailed {
                binary_path,
                status,
                stderr,
            } => write!(
                f,
                "EasyTier probe {} exited with {status}: {}",
                binary_path.display(),
                stderr.trim()
            ),
        }
    }
}

impl Error for EasyTierProbeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SpawnFailed { source, .. } => Some(source),
            Self::TimedOut { .. } | Self::ExitFailed { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierRestartBackoff {
    attempt: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl Default for EasyTierRestartBackoff {
    fn default() -> Self {
        Self {
            attempt: 0,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl EasyTierRestartBackoff {
    pub fn new(base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            attempt: 0,
            base_delay,
            max_delay,
        }
    }

    pub fn next_delay(&self) -> Duration {
        let multiplier = 1u32.checked_shl(self.attempt.min(30)).unwrap_or(u32::MAX);
        self.base_delay
            .saturating_mul(multiplier)
            .min(self.max_delay)
    }

    pub fn record_failure(&mut self) {
        self.attempt = self.attempt.saturating_add(1);
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EasyTierHealthMonitorConfig {
    pub probe: Option<EasyTierCliProbeConfig>,
    pub poll_interval: Duration,
    pub restart_backoff: EasyTierRestartBackoff,
    pub restart_on_exit: bool,
}

impl EasyTierHealthMonitorConfig {
    pub fn from_sidecar_binary(sidecar_binary_path: &Path) -> Self {
        Self {
            probe: Some(EasyTierCliProbeConfig::from_sidecar_binary(
                sidecar_binary_path,
            )),
            ..Self::default()
        }
    }
}

impl Default for EasyTierHealthMonitorConfig {
    fn default() -> Self {
        Self {
            probe: None,
            poll_interval: Duration::from_secs(2),
            restart_backoff: EasyTierRestartBackoff::default(),
            restart_on_exit: true,
        }
    }
}

pub struct EasyTierHealthMonitorHandle {
    cancel_tx: broadcast::Sender<()>,
    pub snapshot_rx: watch::Receiver<EasyTierHealthSnapshot>,
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for EasyTierHealthMonitorHandle {
    fn drop(&mut self) {
        let _ = self.cancel_tx.send(());
    }
}

pub fn spawn_easytier_health_monitor(
    mut manager: EasyTierSidecarManager,
    config: EasyTierHealthMonitorConfig,
    initial_virtual_ip: Option<IpAddr>,
) -> EasyTierHealthMonitorHandle {
    let initial_snapshot = manager
        .health_snapshot(initial_virtual_ip)
        .unwrap_or_else(|err| {
            EasyTierHealthSnapshot::degraded(
                EasyTierProcessState::NotStarted,
                initial_virtual_ip,
                format!("EasyTier status check failed: {err}"),
            )
        });
    let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
    let (cancel_tx, cancel_rx) = broadcast::channel(1);
    let task = tokio::spawn(run_easytier_health_monitor(
        manager,
        config,
        initial_virtual_ip,
        snapshot_tx,
        cancel_rx,
    ));

    EasyTierHealthMonitorHandle {
        cancel_tx,
        snapshot_rx,
        _task: task,
    }
}

pub fn spawn_easytier_probe_health_monitor(
    probe: EasyTierCliProbeConfig,
    poll_interval: Duration,
    initial_virtual_ip: Option<IpAddr>,
) -> EasyTierHealthMonitorHandle {
    let initial_process_state = if initial_virtual_ip.is_some() {
        EasyTierProcessState::Running
    } else {
        EasyTierProcessState::NotStarted
    };
    let initial_snapshot =
        EasyTierHealthSnapshot::from_process_and_probe(initial_process_state, initial_virtual_ip);
    let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
    let (cancel_tx, cancel_rx) = broadcast::channel(1);
    let task = tokio::spawn(run_easytier_probe_health_monitor(
        probe,
        poll_interval,
        initial_virtual_ip,
        snapshot_tx,
        cancel_rx,
    ));

    EasyTierHealthMonitorHandle {
        cancel_tx,
        snapshot_rx,
        _task: task,
    }
}

pub fn spawn_easytier_static_health_monitor(
    snapshot: EasyTierHealthSnapshot,
) -> EasyTierHealthMonitorHandle {
    let (snapshot_tx, snapshot_rx) = watch::channel(snapshot);
    let (cancel_tx, mut cancel_rx) = broadcast::channel(1);
    let task = tokio::spawn(async move {
        let _snapshot_tx = snapshot_tx;
        let _ = cancel_rx.recv().await;
    });

    EasyTierHealthMonitorHandle {
        cancel_tx,
        snapshot_rx,
        _task: task,
    }
}

pub struct EasyTierSidecarManager {
    config: EasyTierSidecarConfig,
    child: Option<tokio::process::Child>,
    last_exit_status: Option<ExitStatus>,
    last_diagnostic: Option<String>,
}

impl EasyTierSidecarManager {
    pub fn new(config: EasyTierSidecarConfig) -> Self {
        Self {
            config,
            child: None,
            last_exit_status: None,
            last_diagnostic: None,
        }
    }

    pub fn from_locator(
        mesh: MeshConfig,
        locator: &EasyTierBinaryLocator,
    ) -> Result<Self, EasyTierSidecarRuntimeError> {
        mesh.validate()
            .map_err(EasyTierSidecarRuntimeError::InvalidMeshConfig)?;
        let binary = locator
            .locate()
            .map_err(EasyTierSidecarRuntimeError::BinaryLocate)?;
        Ok(Self::new(EasyTierSidecarConfig::new(binary.path, mesh)))
    }

    pub fn config(&self) -> &EasyTierSidecarConfig {
        &self.config
    }

    pub fn set_log_file_path(&mut self, path: impl Into<PathBuf>) {
        self.config.log_file_path = Some(path.into());
    }

    pub fn launch_plan(&self) -> EasyTierSidecarLaunchPlan {
        self.config.launch_plan()
    }

    pub fn has_child(&self) -> bool {
        self.child.is_some()
    }

    pub fn process_state(&mut self) -> Result<EasyTierProcessState, EasyTierSidecarRuntimeError> {
        if self.refresh_child_status()? {
            return Ok(EasyTierProcessState::Running);
        }

        Ok(self
            .last_exit_status
            .as_ref()
            .map(|status| EasyTierProcessState::Exited(status.code()))
            .unwrap_or(EasyTierProcessState::NotStarted))
    }

    pub fn health_snapshot(
        &mut self,
        virtual_ip: Option<IpAddr>,
    ) -> Result<EasyTierHealthSnapshot, EasyTierSidecarRuntimeError> {
        let process_state = self.process_state()?;
        Ok(EasyTierHealthSnapshot::from_process_probe_and_diagnostic(
            process_state,
            virtual_ip,
            self.last_diagnostic.as_deref(),
        ))
    }

    pub async fn start(&mut self) -> Result<(), EasyTierSidecarRuntimeError> {
        if self.refresh_child_status()? {
            return Err(EasyTierSidecarRuntimeError::AlreadyRunning);
        }

        self.config
            .mesh
            .validate()
            .map_err(EasyTierSidecarRuntimeError::InvalidMeshConfig)?;

        let mut command = tokio::process::Command::new(&self.config.binary_path);
        command.args(self.config.command_args()).kill_on_drop(true);
        configure_sidecar_stdio(&mut command, self.config.log_file_path.as_deref());

        let child = command
            .spawn()
            .map_err(|source| EasyTierSidecarRuntimeError::SpawnFailed {
                binary_path: self.config.binary_path.clone(),
                source,
            })?;
        self.last_exit_status = None;
        self.last_diagnostic = None;
        self.child = Some(child);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<(), EasyTierSidecarRuntimeError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };

        if child
            .try_wait()
            .map_err(EasyTierSidecarRuntimeError::StatusFailed)?
            .is_some()
        {
            return Ok(());
        }

        child
            .start_kill()
            .map_err(EasyTierSidecarRuntimeError::StopFailed)?;
        let _ = child.wait().await;
        Ok(())
    }

    fn refresh_child_status(&mut self) -> Result<bool, EasyTierSidecarRuntimeError> {
        let Some(child) = &mut self.child else {
            return Ok(false);
        };

        match child
            .try_wait()
            .map_err(EasyTierSidecarRuntimeError::StatusFailed)?
        {
            Some(status) => {
                self.last_exit_status = Some(status);
                self.last_diagnostic = self
                    .config
                    .log_file_path
                    .as_deref()
                    .and_then(read_recent_sidecar_log);
                self.child = None;
                Ok(false)
            }
            None => Ok(true),
        }
    }
}

async fn run_easytier_health_monitor(
    mut manager: EasyTierSidecarManager,
    mut config: EasyTierHealthMonitorConfig,
    mut virtual_ip: Option<IpAddr>,
    snapshot_tx: watch::Sender<EasyTierHealthSnapshot>,
    mut cancel_rx: broadcast::Receiver<()>,
) {
    let poll_interval = if config.poll_interval.is_zero() {
        Duration::from_millis(1)
    } else {
        config.poll_interval
    };
    let mut interval = tokio::time::interval(poll_interval);

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => {
                let _ = manager.stop().await;
                break;
            }
            _ = interval.tick() => {}
        }

        let process_state = match manager.process_state() {
            Ok(state) => state,
            Err(err) => {
                let _ = snapshot_tx.send(EasyTierHealthSnapshot::degraded(
                    EasyTierProcessState::NotStarted,
                    virtual_ip,
                    format!("EasyTier status check failed: {err}"),
                ));
                continue;
            }
        };

        if process_state == EasyTierProcessState::Running {
            if let Some(probe) = &config.probe {
                tokio::select! {
                    result = probe.run() => {
                        if let Ok(result) = result
                            && let Some(ip) = result.virtual_ip
                        {
                            virtual_ip = Some(ip);
                        }
                    }
                    _ = cancel_rx.recv() => {
                        let _ = manager.stop().await;
                        break;
                    }
                }
            }

            let snapshot = match manager.health_snapshot(virtual_ip) {
                Ok(snapshot) => snapshot,
                Err(err) => EasyTierHealthSnapshot::degraded(
                    EasyTierProcessState::NotStarted,
                    virtual_ip,
                    format!("EasyTier status check failed: {err}"),
                ),
            };
            if snapshot.state == EasyTierHealthState::Ready {
                config.restart_backoff.reset();
            }
            let _ = snapshot_tx.send(snapshot);
            continue;
        }

        let snapshot = match manager.health_snapshot(virtual_ip) {
            Ok(snapshot) => snapshot,
            Err(err) => EasyTierHealthSnapshot::degraded(
                EasyTierProcessState::NotStarted,
                virtual_ip,
                format!("EasyTier status check failed: {err}"),
            ),
        };
        let _ = snapshot_tx.send(snapshot);
        if !config.restart_on_exit {
            continue;
        }

        let delay = config.restart_backoff.next_delay();
        config.restart_backoff.record_failure();
        if sleep_or_cancel(delay, &mut cancel_rx).await {
            let _ = manager.stop().await;
            break;
        }

        if let Err(err) = manager.start().await {
            let _ = snapshot_tx.send(EasyTierHealthSnapshot::degraded(
                process_state,
                virtual_ip,
                format!("EasyTier restart failed: {err}"),
            ));
        }
    }
}

async fn run_easytier_probe_health_monitor(
    probe: EasyTierCliProbeConfig,
    poll_interval: Duration,
    mut virtual_ip: Option<IpAddr>,
    snapshot_tx: watch::Sender<EasyTierHealthSnapshot>,
    mut cancel_rx: broadcast::Receiver<()>,
) {
    let poll_interval = if poll_interval.is_zero() {
        Duration::from_millis(1)
    } else {
        poll_interval
    };
    let mut interval = tokio::time::interval(poll_interval);

    loop {
        tokio::select! {
            _ = cancel_rx.recv() => break,
            _ = interval.tick() => {}
        }

        let snapshot = match probe.run().await {
            Ok(result) => {
                if let Some(ip) = result.virtual_ip {
                    virtual_ip = Some(ip);
                }
                EasyTierHealthSnapshot::from_process_and_probe(
                    EasyTierProcessState::Running,
                    virtual_ip,
                )
            }
            Err(err) => EasyTierHealthSnapshot::degraded(
                EasyTierProcessState::NotStarted,
                virtual_ip,
                format!("EasyTier daemon probe failed: {err}"),
            ),
        };
        let _ = snapshot_tx.send(snapshot);
    }
}

async fn sleep_or_cancel(delay: Duration, cancel_rx: &mut broadcast::Receiver<()>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = cancel_rx.recv() => true,
    }
}

impl Drop for EasyTierSidecarManager {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

#[derive(Debug)]
pub enum EasyTierSidecarRuntimeError {
    BinaryLocate(EasyTierBinaryLocateError),
    InvalidMeshConfig(MeshConfigError),
    AlreadyRunning,
    SpawnFailed {
        binary_path: PathBuf,
        source: io::Error,
    },
    StatusFailed(io::Error),
    StopFailed(io::Error),
}

impl fmt::Display for EasyTierSidecarRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BinaryLocate(err) => write!(f, "{err}"),
            Self::InvalidMeshConfig(err) => write!(f, "{err}"),
            Self::AlreadyRunning => f.write_str("EasyTier sidecar is already running"),
            Self::SpawnFailed {
                binary_path,
                source,
            } => write!(
                f,
                "failed to start EasyTier sidecar {}: {source}",
                binary_path.display()
            ),
            Self::StatusFailed(err) => write!(f, "failed to read EasyTier sidecar status: {err}"),
            Self::StopFailed(err) => write!(f, "failed to stop EasyTier sidecar: {err}"),
        }
    }
}

impl Error for EasyTierSidecarRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BinaryLocate(err) => Some(err),
            Self::InvalidMeshConfig(err) => Some(err),
            Self::SpawnFailed { source, .. } => Some(source),
            Self::StatusFailed(err) | Self::StopFailed(err) => Some(err),
            Self::AlreadyRunning => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshConfigError {
    EmptyNetworkName,
    EmptyNetworkSecret,
    EmptyNodeId,
    EmptyDisplayName,
    EmptyPeerEndpoint,
    InvalidPeerEndpoint(String),
    InvalidInviteCode,
}

impl fmt::Display for MeshConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNetworkName => f.write_str("mesh network name is empty"),
            Self::EmptyNetworkSecret => f.write_str("mesh network secret is empty"),
            Self::EmptyNodeId => f.write_str("mesh node id is empty"),
            Self::EmptyDisplayName => f.write_str("mesh display name is empty"),
            Self::EmptyPeerEndpoint => f.write_str("mesh peer endpoint is empty"),
            Self::InvalidPeerEndpoint(endpoint) => {
                write!(f, "mesh peer endpoint is invalid: {endpoint}")
            }
            Self::InvalidInviteCode => f.write_str("mesh invite code is invalid"),
        }
    }
}

impl Error for MeshConfigError {}

pub fn parse_virtual_ip_from_probe_text(value: &str) -> Option<IpAddr> {
    value
        .lines()
        .filter(|line| line_looks_like_virtual_ip_field(line))
        .find_map(find_first_valid_virtual_ip)
}

fn line_looks_like_virtual_ip_field(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    (lower.contains("virtual") && lower.contains("ip"))
        || lower.contains("ipv4")
        || lower.contains("ipv6")
        || lower.contains("tun")
        || lower.contains("address")
}

fn find_first_valid_virtual_ip(line: &str) -> Option<IpAddr> {
    line.split(|ch: char| !(ch.is_ascii_hexdigit() || matches!(ch, '.' | ':' | '/')))
        .filter_map(normalize_ip_token)
        .find(|ip| is_usable_virtual_ip(*ip))
}

fn normalize_ip_token(token: &str) -> Option<IpAddr> {
    let token = token.split_once('/').map(|(addr, _)| addr).unwrap_or(token);
    token.parse::<IpAddr>().ok()
}

fn is_usable_virtual_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            !addr.is_unspecified()
                && !addr.is_loopback()
                && !addr.is_multicast()
                && !addr.is_broadcast()
                && !addr.is_documentation()
        }
        IpAddr::V6(addr) => {
            !addr.is_unspecified()
                && !addr.is_loopback()
                && !addr.is_multicast()
                && !is_ipv6_documentation(addr)
        }
    }
}

fn is_ipv6_documentation(addr: std::net::Ipv6Addr) -> bool {
    let segments = addr.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

const MESH_CONFIG_METADATA_VERSION: &str = "remote-play-mesh-v1";
const MESH_CONFIG_PLACEHOLDER_SECRET: &str = "metadata-secret-loaded-separately";

fn encode_mesh_config_metadata(config: &MeshConfig) -> String {
    let mut out = String::new();
    out.push_str("version=");
    out.push_str(MESH_CONFIG_METADATA_VERSION);
    out.push('\n');
    out.push_str("network_name_hex=");
    out.push_str(&hex_bytes(config.network_name.as_bytes()));
    out.push('\n');
    out.push_str("node_id_hex=");
    out.push_str(&hex_bytes(config.node_id.as_bytes()));
    out.push('\n');
    out.push_str("display_name_hex=");
    out.push_str(&hex_bytes(config.display_name.as_bytes()));
    out.push('\n');
    out.push_str("auto_start=");
    out.push_str(if config.auto_start { "1" } else { "0" });
    out.push('\n');
    out.push_str("integration_mode=");
    out.push_str(mesh_integration_mode_wire(config.integration_mode));
    out.push('\n');
    for peer in &config.initial_peers {
        out.push_str("initial_peer_hex=");
        out.push_str(&hex_bytes(peer.as_str().as_bytes()));
        out.push('\n');
    }
    out
}

fn decode_mesh_config_metadata(value: &str) -> Result<MeshConfig, MeshStoreError> {
    let mut version = None;
    let mut network_name = None;
    let mut node_id = None;
    let mut display_name = None;
    let mut auto_start = None;
    let mut integration_mode = None;
    let mut initial_peers = Vec::new();

    for (line_idx, raw_line) in value.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once('=') else {
            return Err(MeshStoreError::InvalidMetadata(format!(
                "line {} is missing '='",
                line_idx + 1
            )));
        };
        match key {
            "version" => version = Some(raw_value.to_string()),
            "network_name_hex" => network_name = Some(decode_hex_string(raw_value)?),
            "node_id_hex" => node_id = Some(decode_hex_string(raw_value)?),
            "display_name_hex" => display_name = Some(decode_hex_string(raw_value)?),
            "auto_start" => auto_start = Some(decode_bool(raw_value)?),
            "integration_mode" => integration_mode = Some(decode_mesh_integration_mode(raw_value)?),
            "initial_peer_hex" => {
                let peer = decode_hex_string(raw_value)?;
                initial_peers
                    .push(MeshPeerEndpoint::new(peer).map_err(MeshStoreError::InvalidMeshConfig)?);
            }
            other => {
                return Err(MeshStoreError::InvalidMetadata(format!(
                    "unknown key '{other}'"
                )));
            }
        }
    }

    if version.as_deref() != Some(MESH_CONFIG_METADATA_VERSION) {
        return Err(MeshStoreError::InvalidMetadata(
            "unsupported or missing version".to_string(),
        ));
    }

    Ok(MeshConfig {
        network_name: required_metadata_string("network_name_hex", network_name)?,
        network_secret: MeshSecret::new(MESH_CONFIG_PLACEHOLDER_SECRET)
            .map_err(MeshStoreError::InvalidMeshConfig)?,
        node_id: required_metadata_string("node_id_hex", node_id)?,
        display_name: required_metadata_string("display_name_hex", display_name)?,
        initial_peers,
        auto_start: auto_start
            .ok_or_else(|| MeshStoreError::InvalidMetadata("missing auto_start".to_string()))?,
        integration_mode: integration_mode.ok_or_else(|| {
            MeshStoreError::InvalidMetadata("missing integration_mode".to_string())
        })?,
    })
}

fn required_metadata_string(
    key: &'static str,
    value: Option<String>,
) -> Result<String, MeshStoreError> {
    value.ok_or_else(|| MeshStoreError::InvalidMetadata(format!("missing {key}")))
}

fn mesh_integration_mode_wire(mode: MeshIntegrationMode) -> &'static str {
    match mode {
        MeshIntegrationMode::BundledEasyTierSidecar => "bundled-easytier-sidecar",
    }
}

fn decode_mesh_integration_mode(value: &str) -> Result<MeshIntegrationMode, MeshStoreError> {
    match value {
        "bundled-easytier-sidecar" => Ok(MeshIntegrationMode::BundledEasyTierSidecar),
        other => Err(MeshStoreError::InvalidMetadata(format!(
            "unknown integration_mode '{other}'"
        ))),
    }
}

fn decode_bool(value: &str) -> Result<bool, MeshStoreError> {
    match value {
        "1" => Ok(true),
        "0" => Ok(false),
        other => Err(MeshStoreError::InvalidMetadata(format!(
            "invalid bool '{other}'"
        ))),
    }
}

fn decode_hex_string(value: &str) -> Result<String, MeshStoreError> {
    let bytes = decode_hex_bytes(value)?;
    String::from_utf8(bytes)
        .map_err(|_| MeshStoreError::InvalidMetadata("hex value is not valid utf-8".to_string()))
}

fn decode_hex_bytes(value: &str) -> Result<Vec<u8>, MeshStoreError> {
    if !value.len().is_multiple_of(2) {
        return Err(MeshStoreError::InvalidMetadata(
            "hex value has odd length".to_string(),
        ));
    }

    let mut bytes = Vec::with_capacity(value.len() / 2);
    let raw = value.as_bytes();
    for idx in (0..raw.len()).step_by(2) {
        let high = decode_hex_nibble(raw[idx])?;
        let low = decode_hex_nibble(raw[idx + 1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn decode_hex_nibble(value: u8) -> Result<u8, MeshStoreError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(MeshStoreError::InvalidMetadata(
            "hex value contains non-hex digit".to_string(),
        )),
    }
}

fn create_private_dir(path: &Path) -> Result<(), MeshStoreError> {
    fs::create_dir_all(path).map_err(|source| MeshStoreError::io(path.to_path_buf(), source))?;
    set_private_dir_permissions(path)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), MeshStoreError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        create_private_dir(parent)?;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("mesh-store");
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp-{}", hex_random::<4>()));

    let write_result = fs::write(&tmp_path, bytes)
        .map_err(|source| MeshStoreError::io(tmp_path.clone(), source))
        .and_then(|_| set_private_file_permissions(&tmp_path))
        .and_then(|_| {
            fs::rename(&tmp_path, path)
                .map_err(|source| MeshStoreError::io(path.to_path_buf(), source))
        })
        .and_then(|_| set_private_file_permissions(path));

    if write_result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }

    write_result
}

fn remove_file_if_exists(path: &Path) -> Result<(), MeshStoreError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MeshStoreError::io(path.to_path_buf(), source)),
    }
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<(), MeshStoreError> {
    let permissions = fs::Permissions::from_mode(0o700);
    fs::set_permissions(path, permissions)
        .map_err(|source| MeshStoreError::io(path.to_path_buf(), source))
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), MeshStoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), MeshStoreError> {
    let permissions = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, permissions)
        .map_err(|source| MeshStoreError::io(path.to_path_buf(), source))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), MeshStoreError> {
    Ok(())
}

fn validate_binary_candidate(
    candidate: EasyTierBinaryCandidate,
) -> Result<EasyTierBinaryLocation, EasyTierBinaryLocateError> {
    let Ok(metadata) = fs::metadata(&candidate.path) else {
        return Err(EasyTierBinaryLocateError::NotFound {
            binary_name: candidate
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(DEFAULT_EASYTIER_BINARY_NAME)
                .to_string(),
            searched: vec![candidate.path],
        });
    };

    if !metadata.is_file() {
        return Err(EasyTierBinaryLocateError::NotAFile(candidate.path));
    }
    if !is_executable(&metadata) {
        return Err(EasyTierBinaryLocateError::NotExecutable(candidate.path));
    }

    Ok(EasyTierBinaryLocation {
        source: candidate.source,
        path: candidate.path,
    })
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

fn non_empty_env_path(value: std::ffi::OsString) -> Option<std::ffi::OsString> {
    if value.to_string_lossy().trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

fn default_app_resource_dirs(current_exe: Option<&Path>) -> Vec<PathBuf> {
    let Some(current_exe) = current_exe else {
        return Vec::new();
    };
    let Some(exe_dir) = current_exe.parent() else {
        return Vec::new();
    };

    let mut dirs = vec![
        exe_dir.join("bin"),
        exe_dir.join("resources"),
        exe_dir.join("Resources"),
    ];

    if exe_dir.file_name().is_some_and(|name| name == "MacOS")
        && let Some(contents_dir) = exe_dir.parent()
        && contents_dir
            .file_name()
            .is_some_and(|name| name == "Contents")
    {
        dirs.push(contents_dir.join("Resources"));
        dirs.push(contents_dir.join("Resources").join("bin"));
    }

    dedup_paths(dirs)
}

fn dedup_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn dedup_candidates(
    candidates: impl IntoIterator<Item = EasyTierBinaryCandidate>,
) -> Vec<EasyTierBinaryCandidate> {
    let mut out = Vec::new();
    for candidate in candidates {
        if !out
            .iter()
            .any(|existing: &EasyTierBinaryCandidate| existing.path == candidate.path)
        {
            out.push(candidate);
        }
    }
    out
}

fn sanitized_display_name(display_name: impl Into<String>) -> String {
    let display_name = display_name.into();
    let display_name = display_name.trim();
    if display_name.is_empty() {
        "RemotePlay Device".to_string()
    } else {
        display_name.to_string()
    }
}

fn derive_virtual_ipv4(network_name: &str, node_id: &str) -> Ipv4Addr {
    let network_hash = fnv1a32(network_name.as_bytes());
    let node_hash = fnv1a32(node_id.as_bytes());
    let second = 128 + ((network_hash >> 8) & 0x3f) as u8;
    let third = (network_hash & 0xff) as u8;
    let mut fourth = (node_hash & 0xff) as u8;
    if fourth == 0 {
        fourth = 1;
    } else if fourth == 255 {
        fourth = 254;
    }
    Ipv4Addr::new(10, second, third, fourth)
}

fn hex_random<const N: usize>() -> String {
    hex_bytes(&random::<[u8; N]>())
}

fn hex_bytes(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_mesh_config_is_valid_and_redacts_secret() {
        let config = MeshConfig::generate("Alice Mac");

        config.validate().expect("generated config is valid");
        assert!(config.network_name.starts_with("remote-play-"));
        assert_eq!(config.display_name, "Alice Mac");
        assert_eq!(
            config.initial_peers[0].as_str(),
            DEFAULT_EASYTIER_PUBLIC_PEER
        );
        assert!(
            !format!("{:?}", config.network_secret).contains(config.network_secret.expose_secret())
        );
    }

    #[test]
    fn invite_code_roundtrips_network_credentials_without_node_identity() {
        let config = MeshConfig::generate("Host");
        let invite_code = config.invite_code();
        assert!(invite_code.starts_with(REMOTE_PLAY_MESH_COPY_CODE_PREFIX));
        assert!(!invite_code.contains(config.network_secret.expose_secret()));
        assert!(!invite_code.contains(DEFAULT_EASYTIER_PUBLIC_PEER));

        let joined =
            MeshConfig::from_invite_code(&invite_code, "Viewer").expect("invite should decode");

        assert_eq!(joined.network_name, config.network_name);
        assert_eq!(
            joined.network_secret.expose_secret(),
            config.network_secret.expose_secret()
        );
        assert_eq!(joined.initial_peers, config.initial_peers);
        assert_eq!(joined.display_name, "Viewer");
        assert_ne!(joined.node_id, config.node_id);
    }

    #[test]
    fn legacy_invite_code_still_decodes_for_compatibility() {
        let config = MeshConfig::generate("Host");
        let invite_code = config.legacy_invite_code();
        assert!(invite_code.starts_with(REMOTE_PLAY_MESH_INVITE_PREFIX));

        let joined = MeshConfig::from_invite_code(&invite_code, "Viewer")
            .expect("legacy invite should decode");
        assert_eq!(joined.network_name, config.network_name);
        assert_eq!(
            joined.network_secret.expose_secret(),
            config.network_secret.expose_secret()
        );
        assert_eq!(joined.initial_peers, config.initial_peers);
    }

    #[test]
    fn invite_code_rejects_invalid_payloads() {
        assert_eq!(
            MeshInvite::decode("not-an-invite").unwrap_err(),
            MeshConfigError::InvalidInviteCode
        );
        assert_eq!(
            MeshInvite::decode("rpmesh1||secret|tcp://public.easytier.top:11010").unwrap_err(),
            MeshConfigError::EmptyNetworkName
        );
    }

    #[test]
    fn copy_code_tolerates_case_whitespace_and_grouping() {
        let config = MeshConfig::generate("Host");
        let invite_code = config.invite_code();
        let noisy_code = invite_code
            .chars()
            .enumerate()
            .map(|(idx, ch)| {
                if idx % 11 == 0 {
                    format!(" \n{}", ch.to_ascii_lowercase())
                } else {
                    ch.to_ascii_lowercase().to_string()
                }
            })
            .collect::<String>();

        let joined = MeshConfig::from_invite_code(&noisy_code, "Viewer")
            .expect("copy code should decode after formatting noise");
        assert_eq!(joined.network_name, config.network_name);
        assert_eq!(
            joined.network_secret.expose_secret(),
            config.network_secret.expose_secret()
        );
        assert_eq!(joined.initial_peers, config.initial_peers);
    }

    #[test]
    fn copy_code_roundtrips_non_default_peers_and_utf8_secret() {
        let invite = MeshInvite {
            network_name: "remote-play-custom".to_string(),
            network_secret: MeshSecret::new("not-hex-secret").unwrap(),
            initial_peers: vec![
                MeshPeerEndpoint::new("tcp://10.0.0.2:11010").unwrap(),
                MeshPeerEndpoint::new("udp://10.0.0.3:11010").unwrap(),
            ],
        };

        let code = invite.encode_copy_code();
        let decoded = MeshInvite::decode(&code).expect("copy code should decode");
        assert_eq!(decoded, invite);
    }

    #[test]
    fn copy_code_rejects_checksum_changes() {
        let config = MeshConfig::generate("Host");
        let mut code = config.invite_code();
        let last = code.pop().expect("code has content");
        code.push(if last == 'A' { 'B' } else { 'A' });

        assert_eq!(
            MeshInvite::decode(&code).unwrap_err(),
            MeshConfigError::InvalidInviteCode
        );
    }

    #[test]
    fn mesh_config_metadata_roundtrips_without_serializing_secret() {
        let mut config = MeshConfig::generate("Alice Laptop");
        config.auto_start = false;
        config
            .initial_peers
            .push(MeshPeerEndpoint::new("tcp://10.0.0.2:11010").unwrap());
        let secret = config.network_secret.expose_secret().to_string();
        let secret_hex = hex_bytes(secret.as_bytes());

        let encoded = encode_mesh_config_metadata(&config);
        assert!(!encoded.contains(&secret));
        assert!(!encoded.contains(&secret_hex));

        let decoded = decode_mesh_config_metadata(&encoded).expect("metadata should decode");
        assert_eq!(decoded.network_name, config.network_name);
        assert_eq!(decoded.node_id, config.node_id);
        assert_eq!(decoded.display_name, config.display_name);
        assert_eq!(decoded.initial_peers, config.initial_peers);
        assert_eq!(decoded.auto_start, config.auto_start);
        assert_eq!(decoded.integration_mode, config.integration_mode);
        assert_ne!(
            decoded.network_secret.expose_secret(),
            config.network_secret.expose_secret()
        );
    }

    #[test]
    fn app_private_mesh_store_roundtrips_config_and_secret() {
        let temp = TempTree::new("mesh-store");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        let config = MeshConfig::generate("Desk");
        let secret = config.network_secret.expose_secret().to_string();
        let secret_hex = hex_bytes(secret.as_bytes());

        assert!(
            store
                .load()
                .expect("missing config should not fail")
                .is_none()
        );
        store.save(&config).expect("save config");

        let metadata = fs::read_to_string(store.config_path()).expect("read metadata");
        assert!(!metadata.contains(&secret));
        assert!(!metadata.contains(&secret_hex));
        assert_eq!(
            fs::read_to_string(store.secret_store().path()).expect("read secret"),
            secret
        );

        let loaded = store
            .load()
            .expect("load should succeed")
            .expect("config should exist");
        assert_eq!(loaded, config);

        store.delete().expect("delete stored config");
        assert!(
            store
                .load()
                .expect("deleted config should not fail")
                .is_none()
        );
        assert!(!store.secret_store().path().exists());
    }

    #[test]
    fn app_private_mesh_store_can_load_or_generate_first_run_config() {
        let temp = TempTree::new("mesh-load-or-generate");
        let store = AppPrivateMeshConfigStore::new(&temp.root);

        let generated = store
            .load_or_generate("First Run")
            .expect("first run should generate config");
        assert_eq!(generated.display_name, "First Run");
        assert!(store.config_path().exists());
        assert!(store.secret_store().path().exists());

        let loaded = store
            .load_or_generate("Ignored Name")
            .expect("second run should load existing config");
        assert_eq!(loaded, generated);
    }

    #[test]
    fn app_private_mesh_store_migrates_legacy_default_public_peer() {
        let temp = TempTree::new("mesh-migrate-peer");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        let mut config = MeshConfig::generate("Desk");
        config.initial_peers = vec![MeshPeerEndpoint::new(LEGACY_EASYTIER_PUBLIC_PEER).unwrap()];
        let secret = config.network_secret.expose_secret().to_string();

        store.save(&config).expect("save legacy config");

        let loaded = store
            .load_or_generate("Ignored Name")
            .expect("load should migrate legacy peer");
        assert_eq!(loaded.network_secret.expose_secret(), secret);
        assert_eq!(loaded.initial_peers.len(), 1);
        assert_eq!(
            loaded.initial_peers[0].as_str(),
            DEFAULT_EASYTIER_PUBLIC_PEER
        );

        let reloaded = store
            .load()
            .expect("migrated config should load")
            .expect("config should exist");
        assert_eq!(reloaded.initial_peers, loaded.initial_peers);
    }

    #[test]
    fn app_private_mesh_store_reports_missing_secret() {
        let temp = TempTree::new("mesh-missing-secret");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        let config = MeshConfig::generate("Desk");
        write_private_file(
            store.config_path(),
            encode_mesh_config_metadata(&config).as_bytes(),
        )
        .expect("write metadata");

        let err = store.load().unwrap_err();
        assert!(matches!(err, MeshStoreError::MissingNetworkSecret));
    }

    #[test]
    fn app_private_mesh_store_rejects_invalid_metadata() {
        let temp = TempTree::new("mesh-invalid-metadata");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        write_private_file(
            store.config_path(),
            b"version=remote-play-mesh-v1\nnetwork_name_hex=not-hex\n",
        )
        .expect("write invalid metadata");
        store
            .secret_store()
            .save_secret(&MeshSecret::new("secret").unwrap())
            .expect("write secret");

        let err = store.load().unwrap_err();
        assert!(matches!(err, MeshStoreError::InvalidMetadata(_)));
    }

    #[cfg(unix)]
    #[test]
    fn app_private_mesh_store_uses_restrictive_fallback_permissions() {
        let temp = TempTree::new("mesh-permissions");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        let config = MeshConfig::generate("Desk");

        store.save(&config).expect("save config");

        let root_mode = fs::metadata(store.root_dir())
            .expect("root metadata")
            .permissions()
            .mode()
            & 0o777;
        let config_mode = fs::metadata(store.config_path())
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;
        let secret_mode = fs::metadata(store.secret_store().path())
            .expect("secret metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(root_mode, 0o700);
        assert_eq!(config_mode, 0o600);
        assert_eq!(secret_mode, 0o600);
    }

    #[test]
    fn sidecar_command_args_are_safe_to_log_without_debugging_the_secret() {
        let config = MeshConfig::generate("Desk");
        let secret = config.network_secret.expose_secret().to_string();
        let sidecar = EasyTierSidecarConfig::new("easytier-core", config);
        let args = sidecar.command_args();
        let redacted_args = sidecar.redacted_command_args();
        let launch_plan = sidecar.launch_plan();

        assert!(
            args.windows(2)
                .any(|pair| pair[0].as_str() == "--network-name"
                    && pair[1].as_str() == sidecar.mesh.network_name.as_str())
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0].as_str() == "--network-secret" && pair[1].as_str() == secret)
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0].as_str() == "--hostname" && pair[1].as_str() == "Desk")
        );
        assert!(args.windows(2).any(|pair| pair[0].as_str() == "--ipv4"
            && pair[1].as_str() == sidecar.virtual_ipv4.expect("virtual ipv4").to_string()));
        assert!(!args.iter().any(|arg| arg == "--dhcp"));
        assert!(
            args.windows(2)
                .any(|pair| pair[0].as_str() == "--latency-first" && pair[1].as_str() == "true")
        );
        assert!(!args.iter().any(|arg| arg == "--private-mode"));
        assert!(args.windows(2).any(
            |pair| pair[0].as_str() == "--rpc-portal" && pair[1].as_str() == "127.0.0.1:15888"
        ));
        assert!(args.windows(2).any(
            |pair| pair[0].as_str() == "-p" && pair[1].as_str() == DEFAULT_EASYTIER_PUBLIC_PEER
        ));
        assert!(!format!("{sidecar:?}").contains(&secret));
        assert!(!format!("{launch_plan:?}").contains(&secret));
        assert!(args.iter().any(|arg| arg == &secret));
        assert!(!redacted_args.iter().any(|arg| arg == &secret));
        assert!(redacted_args.iter().any(|arg| arg == "<redacted>"));
    }

    #[test]
    fn virtual_ip_parser_accepts_common_easytier_node_output_shapes() {
        assert_eq!(
            parse_virtual_ip_from_probe_text("Virtual IP: 10.144.0.12/24\n"),
            Some("10.144.0.12".parse().unwrap())
        );
        assert_eq!(
            parse_virtual_ip_from_probe_text(r#"{ "ipv4": "10.1.2.3", "hostname": "desk" }"#),
            Some("10.1.2.3".parse().unwrap())
        );
        assert_eq!(
            parse_virtual_ip_from_probe_text("tun address fe80::1234\n"),
            Some("fe80::1234".parse().unwrap())
        );
    }

    #[test]
    fn virtual_ip_parser_ignores_unusable_addresses_and_unrelated_peers() {
        assert_eq!(
            parse_virtual_ip_from_probe_text(
                "peer tcp://public.easytier.top:11010\nVirtual IP: 127.0.0.1\n"
            ),
            None
        );
        assert_eq!(
            parse_virtual_ip_from_probe_text(
                "peers: 8.8.8.8\nVirtual IP: 192.0.2.1\nAddress: 10.9.8.7"
            ),
            Some("10.9.8.7".parse().unwrap())
        );
    }

    #[test]
    fn health_snapshot_tracks_running_ready_and_exited_states() {
        let starting =
            EasyTierHealthSnapshot::from_process_and_probe(EasyTierProcessState::Running, None);
        assert_eq!(starting.state, EasyTierHealthState::Starting);
        assert_eq!(starting.issue, None);

        let ready = EasyTierHealthSnapshot::from_process_and_probe(
            EasyTierProcessState::Running,
            Some("10.1.1.2".parse().unwrap()),
        );
        assert_eq!(ready.state, EasyTierHealthState::Ready);
        assert_eq!(ready.virtual_ip, Some("10.1.1.2".parse().unwrap()));
        assert_eq!(ready.issue, None);

        let degraded = EasyTierHealthSnapshot::from_process_and_probe(
            EasyTierProcessState::Exited(Some(2)),
            None,
        );
        assert_eq!(degraded.state, EasyTierHealthState::Degraded);
        assert_eq!(degraded.issue, None);
        assert!(degraded.message.contains("2"));
    }

    #[test]
    fn health_snapshot_classifies_tun_permission_failures() {
        let snapshot = EasyTierHealthSnapshot::from_process_probe_and_diagnostic(
            EasyTierProcessState::Exited(Some(1)),
            None,
            Some("tun device error err=rust tun error Operation not permitted (os error 1)"),
        );

        assert_eq!(snapshot.state, EasyTierHealthState::Degraded);
        assert_eq!(
            snapshot.issue,
            Some(EasyTierHealthIssue::RequiresAdminPrivileges)
        );
        assert!(snapshot.message.contains("administrator permission"));
    }

    #[test]
    fn virtual_ipv4_derivation_is_stable_and_private() {
        let first = derive_virtual_ipv4("mesh-a", "node-a");
        let second = derive_virtual_ipv4("mesh-a", "node-a");
        let same_mesh_other_node = derive_virtual_ipv4("mesh-a", "node-b");
        let other_mesh = derive_virtual_ipv4("mesh-b", "node-a");

        assert_eq!(first, second);
        assert_ne!(first, same_mesh_other_node);
        assert_ne!(first, other_mesh);
        assert_eq!(first.octets()[0], 10);
        assert!((128..=191).contains(&first.octets()[1]));
        assert_eq!(&first.octets()[..3], &same_mesh_other_node.octets()[..3]);
        assert_ne!(&first.octets()[..3], &other_mesh.octets()[..3]);
        assert!(!matches!(first.octets()[3], 0 | 255));
    }

    #[test]
    fn restart_backoff_grows_and_caps_until_reset() {
        let mut backoff =
            EasyTierRestartBackoff::new(Duration::from_millis(100), Duration::from_secs(1));

        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
        backoff.record_failure();
        assert_eq!(backoff.next_delay(), Duration::from_millis(200));
        backoff.record_failure();
        assert_eq!(backoff.next_delay(), Duration::from_millis(400));
        for _ in 0..10 {
            backoff.record_failure();
        }
        assert_eq!(backoff.next_delay(), Duration::from_secs(1));
        backoff.reset();
        assert_eq!(backoff.next_delay(), Duration::from_millis(100));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_probe_parses_virtual_ip_from_command_output() {
        let temp = TempTree::new("mesh-cli-probe");
        let cli_path = temp.script(
            "easytier-cli",
            "#!/bin/sh\necho 'Virtual IP: 10.77.0.8/24'\n",
        );
        let probe = EasyTierCliProbeConfig {
            timeout: Duration::from_secs(5),
            ..EasyTierCliProbeConfig::node(cli_path)
        };

        let result = probe.run().await.expect("probe should run");
        assert_eq!(result.virtual_ip, Some("10.77.0.8".parse().unwrap()));
    }

    #[test]
    fn binary_locator_prefers_env_override_and_requires_it_to_exist() {
        let temp = TempTree::new("mesh-env");
        let bin_path = temp.executable(DEFAULT_EASYTIER_BINARY_NAME);
        let path_dir = temp.dir("path");
        let path_bin = temp.executable_in(&path_dir, DEFAULT_EASYTIER_BINARY_NAME);

        let locator = EasyTierBinaryLocator::new(DEFAULT_EASYTIER_BINARY_NAME)
            .with_env_override(&bin_path)
            .with_path_dirs([path_dir]);

        let located = locator.locate().expect("env override should locate");
        assert_eq!(located.source, EasyTierBinarySource::EnvOverride);
        assert_eq!(located.path, bin_path);
        assert_ne!(located.path, path_bin);

        let missing = temp.root.join("missing-easytier-core");
        let err = EasyTierBinaryLocator::new(DEFAULT_EASYTIER_BINARY_NAME)
            .with_env_override(&missing)
            .locate()
            .unwrap_err();
        assert_eq!(err, EasyTierBinaryLocateError::EnvOverrideMissing(missing));
    }

    #[test]
    fn binary_locator_searches_bundled_resources_before_path() {
        let temp = TempTree::new("mesh-resource");
        let resource_dir = temp.dir("resources");
        let path_dir = temp.dir("path");
        let resource_bin = temp.executable_in(&resource_dir, DEFAULT_EASYTIER_BINARY_NAME);
        let path_bin = temp.executable_in(&path_dir, DEFAULT_EASYTIER_BINARY_NAME);

        let locator = EasyTierBinaryLocator::new(DEFAULT_EASYTIER_BINARY_NAME)
            .with_app_resource_dirs([resource_dir])
            .with_path_dirs([path_dir]);

        let located = locator.locate().expect("resource binary should locate");
        assert_eq!(located.source, EasyTierBinarySource::AppResource);
        assert_eq!(located.path, resource_bin);
        assert_ne!(located.path, path_bin);
    }

    #[test]
    fn binary_locator_reports_not_executable_bundle_binary() {
        let temp = TempTree::new("mesh-not-executable");
        let resource_dir = temp.dir("resources");
        let bin_path = resource_dir.join(DEFAULT_EASYTIER_BINARY_NAME);
        fs::write(&bin_path, b"not executable").expect("write fake binary");

        let locator = EasyTierBinaryLocator::new(DEFAULT_EASYTIER_BINARY_NAME)
            .with_app_resource_dirs([resource_dir]);

        #[cfg(unix)]
        {
            let err = locator.locate().unwrap_err();
            assert_eq!(err, EasyTierBinaryLocateError::NotExecutable(bin_path));
        }

        #[cfg(not(unix))]
        {
            let located = locator
                .locate()
                .expect("Windows does not use Unix exec bits");
            assert_eq!(located.path, bin_path);
        }
    }

    #[test]
    fn sidecar_manager_plans_command_from_located_binary_without_leaking_secret() {
        let temp = TempTree::new("mesh-manager");
        let bin_path = temp.executable(DEFAULT_EASYTIER_BINARY_NAME);
        let locator =
            EasyTierBinaryLocator::new(DEFAULT_EASYTIER_BINARY_NAME).with_env_override(&bin_path);
        let mesh = MeshConfig::generate("Laptop");
        let secret = mesh.network_secret.expose_secret().to_string();

        let manager = EasyTierSidecarManager::from_locator(mesh, &locator)
            .expect("manager should use located binary");
        let launch_plan = manager.launch_plan();

        assert_eq!(launch_plan.binary_path, bin_path);
        assert!(launch_plan.args.iter().any(|arg| arg == &secret));
        assert!(!launch_plan.redacted_args.iter().any(|arg| arg == &secret));
        assert!(!format!("{launch_plan:?}").contains(&secret));
        assert!(!manager.has_child());
    }

    #[tokio::test]
    async fn sidecar_manager_can_start_and_stop_a_short_lived_sidecar() {
        let temp = TempTree::new("mesh-lifecycle");
        let bin_path = temp.executable(DEFAULT_EASYTIER_BINARY_NAME);
        let mesh = MeshConfig::generate("Lifecycle");
        let sidecar = EasyTierSidecarConfig::new(bin_path, mesh);
        let mut manager = EasyTierSidecarManager::new(sidecar);

        manager.start().await.expect("sidecar should start");
        manager.stop().await.expect("sidecar should stop cleanly");
        assert!(!manager.has_child());
    }

    #[tokio::test]
    async fn sidecar_manager_reports_exited_process_state() {
        let temp = TempTree::new("mesh-exited-state");
        let bin_path = temp.executable(DEFAULT_EASYTIER_BINARY_NAME);
        let mesh = MeshConfig::generate("Lifecycle");
        let sidecar = EasyTierSidecarConfig::new(bin_path, mesh);
        let mut manager = EasyTierSidecarManager::new(sidecar);

        manager.start().await.expect("sidecar should start");
        let state = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let state = manager.process_state().expect("status should read");
                if state != EasyTierProcessState::Running {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake process should exit quickly");
        assert_eq!(state, EasyTierProcessState::Exited(Some(0)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sidecar_manager_reads_exit_diagnostics_from_log_file() {
        let temp = TempTree::new("mesh-exit-diagnostic");
        let bin_path = temp.script(
            DEFAULT_EASYTIER_BINARY_NAME,
            "#!/bin/sh\necho 'tun device error err=rust tun error Operation not permitted (os error 1)' >&2\nexit 1\n",
        );
        let mesh = MeshConfig::generate("Needs Admin");
        let mut manager = EasyTierSidecarManager::new(EasyTierSidecarConfig::new(bin_path, mesh));
        manager.set_log_file_path(temp.root.join(EASYTIER_SIDECAR_LOG_FILE_NAME));

        manager.start().await.expect("sidecar should start");
        let snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = manager.health_snapshot(None).expect("status should read");
                if snapshot.process_state != EasyTierProcessState::Running {
                    break snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake process should exit quickly");

        assert_eq!(snapshot.state, EasyTierHealthState::Degraded);
        assert_eq!(
            snapshot.issue,
            Some(EasyTierHealthIssue::RequiresAdminPrivileges)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn health_monitor_polls_virtual_ip_and_publishes_ready_snapshot() {
        let temp = TempTree::new("mesh-health-ready");
        let sidecar_path = temp.script(
            DEFAULT_EASYTIER_BINARY_NAME,
            "#!/bin/sh\nwhile true; do sleep 1; done\n",
        );
        let cli_path = temp.script(
            DEFAULT_EASYTIER_CLI_BINARY_NAME,
            "#!/bin/sh\necho 'Virtual IP: 10.88.0.9/24'\n",
        );
        let mesh = MeshConfig::generate("Health");
        let mut manager =
            EasyTierSidecarManager::new(EasyTierSidecarConfig::new(&sidecar_path, mesh));
        manager.start().await.expect("sidecar should start");

        let handle = spawn_easytier_health_monitor(
            manager,
            EasyTierHealthMonitorConfig {
                probe: Some(EasyTierCliProbeConfig {
                    timeout: Duration::from_secs(1),
                    ..EasyTierCliProbeConfig::node(cli_path)
                }),
                poll_interval: Duration::from_millis(20),
                restart_on_exit: false,
                ..EasyTierHealthMonitorConfig::default()
            },
            None,
        );
        let mut snapshot_rx = handle.snapshot_rx.clone();

        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                snapshot_rx.changed().await.expect("snapshot update");
                let snapshot = snapshot_rx.borrow().clone();
                if snapshot.state == EasyTierHealthState::Ready {
                    break snapshot;
                }
            }
        })
        .await
        .expect("monitor should publish ready snapshot");

        assert_eq!(ready.virtual_ip, Some("10.88.0.9".parse().unwrap()));
        drop(handle);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_health_monitor_publishes_external_daemon_snapshot() {
        let temp = TempTree::new("mesh-probe-health-ready");
        let cli_path = temp.script(
            DEFAULT_EASYTIER_CLI_BINARY_NAME,
            "#!/bin/sh\necho 'Virtual IP: 10.99.0.10/24'\n",
        );

        let handle = spawn_easytier_probe_health_monitor(
            EasyTierCliProbeConfig {
                timeout: Duration::from_secs(1),
                ..EasyTierCliProbeConfig::node(cli_path)
            },
            Duration::from_millis(20),
            None,
        );
        let mut snapshot_rx = handle.snapshot_rx.clone();

        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                snapshot_rx.changed().await.expect("snapshot update");
                let snapshot = snapshot_rx.borrow().clone();
                if snapshot.state == EasyTierHealthState::Ready {
                    break snapshot;
                }
            }
        })
        .await
        .expect("probe monitor should publish ready snapshot");

        assert_eq!(ready.process_state, EasyTierProcessState::Running);
        assert_eq!(ready.virtual_ip, Some("10.99.0.10".parse().unwrap()));
        drop(handle);
    }

    #[tokio::test]
    async fn static_health_monitor_publishes_snapshot_without_probe_process() {
        let snapshot = EasyTierHealthSnapshot::from_process_and_probe(
            EasyTierProcessState::Running,
            Some("10.88.0.42".parse().unwrap()),
        );

        let handle = spawn_easytier_static_health_monitor(snapshot.clone());

        assert_eq!(handle.snapshot_rx.borrow().clone(), snapshot);
        drop(handle);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn health_monitor_restarts_exited_sidecar_with_backoff() {
        let temp = TempTree::new("mesh-health-restart");
        let run_log = temp.root.join("runs.log");
        let sidecar_path = temp.script(
            DEFAULT_EASYTIER_BINARY_NAME,
            &format!("#!/bin/sh\necho run >> '{}'\nexit 0\n", run_log.display()),
        );
        let mesh = MeshConfig::generate("Restart");
        let mut manager =
            EasyTierSidecarManager::new(EasyTierSidecarConfig::new(&sidecar_path, mesh));
        manager.start().await.expect("sidecar should start once");

        let handle = spawn_easytier_health_monitor(
            manager,
            EasyTierHealthMonitorConfig {
                poll_interval: Duration::from_millis(10),
                restart_backoff: EasyTierRestartBackoff::new(
                    Duration::from_millis(10),
                    Duration::from_millis(10),
                ),
                restart_on_exit: true,
                ..EasyTierHealthMonitorConfig::default()
            },
            None,
        );

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let run_count = fs::read_to_string(&run_log)
                    .map(|value| value.lines().count())
                    .unwrap_or(0);
                if run_count >= 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("monitor should restart the fake sidecar");
        drop(handle);
    }

    #[test]
    fn binary_locator_reports_missing_locations_without_secrets() {
        let temp = TempTree::new("mesh-missing");
        let resource_dir = temp.dir("resources");
        let path_dir = temp.dir("path");
        let locator = EasyTierBinaryLocator::new(DEFAULT_EASYTIER_BINARY_NAME)
            .with_app_resource_dirs([resource_dir])
            .with_current_exe_dir(temp.dir("exe"))
            .with_path_dirs([path_dir]);

        let err = locator.locate().unwrap_err();
        let EasyTierBinaryLocateError::NotFound {
            binary_name,
            searched,
        } = err
        else {
            panic!("expected not found error");
        };
        assert_eq!(binary_name, DEFAULT_EASYTIER_BINARY_NAME);
        assert_eq!(searched.len(), 3);
        assert!(
            searched
                .iter()
                .all(|path| path.ends_with(DEFAULT_EASYTIER_BINARY_NAME))
        );
    }

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(name: &str) -> Self {
            let root = env::temp_dir().join(format!("remote-play-{name}-{}", hex_random::<8>()));
            fs::create_dir_all(&root).expect("create temp test root");
            Self { root }
        }

        fn dir(&self, name: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::create_dir_all(&path).expect("create temp test dir");
            path
        }

        fn executable(&self, name: &str) -> PathBuf {
            self.executable_in(&self.root, name)
        }

        fn executable_in(&self, dir: &Path, name: &str) -> PathBuf {
            fs::create_dir_all(dir).expect("create executable parent");
            let path = dir.join(name);
            fs::write(&path, executable_stub()).expect("write fake binary");
            make_executable(&path);
            path
        }

        #[cfg(unix)]
        fn script(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.root.join(name);
            fs::write(&path, contents).expect("write fake script");
            make_executable(&path);
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[cfg(unix)]
    fn executable_stub() -> &'static [u8] {
        b"#!/bin/sh\nexit 0\n"
    }

    #[cfg(not(unix))]
    fn executable_stub() -> &'static [u8] {
        b""
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("set executable bit");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}
}
