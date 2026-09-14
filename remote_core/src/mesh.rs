use rand::random;
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub const MESH_CONFIG_FILE_NAME: &str = "mesh.conf";
pub const MESH_SECRET_FILE_NAME: &str = "mesh.secret";
pub const REMOTE_PLAY_DEVICE_GROUP_DIR_ENV: &str = "REMOTE_PLAY_DEVICE_GROUP_DIR";
pub const REMOTE_PLAY_DEVICE_GROUP_COPY_CODE_PREFIX: &str = "RPM2";

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
pub struct MeshConfig {
    pub network_name: String,
    pub network_secret: MeshSecret,
    pub node_id: String,
    pub display_name: String,
}

impl MeshConfig {
    pub fn generate(display_name: impl Into<String>) -> Self {
        Self {
            network_name: format!("remote-play-{}", hex_random::<8>()),
            network_secret: MeshSecret::generate(),
            node_id: hex_random::<16>(),
            display_name: sanitized_display_name(display_name),
        }
    }

    pub fn from_invite_code(
        invite_code: &str,
        display_name: impl Into<String>,
    ) -> Result<Self, MeshConfigError> {
        let invite = MeshInvite::decode(invite_code)?;
        Ok(Self {
            network_name: invite.network_name,
            network_secret: invite.network_secret,
            node_id: hex_random::<16>(),
            display_name: sanitized_display_name(display_name),
        })
    }

    pub fn invite_code(&self) -> String {
        MeshInvite {
            network_name: self.network_name.clone(),
            network_secret: self.network_secret.clone(),
        }
        .encode_copy_code()
    }

    pub fn validate(&self) -> Result<(), MeshConfigError> {
        if self.network_name.trim().is_empty() {
            return Err(MeshConfigError::EmptyNetworkName);
        }
        if self.network_secret.expose_secret().trim().is_empty() {
            return Err(MeshConfigError::EmptyNetworkSecret);
        }
        if self.node_id.trim().is_empty() {
            return Err(MeshConfigError::EmptyNodeId);
        }
        if self.display_name.trim().is_empty() {
            return Err(MeshConfigError::EmptyDisplayName);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshInvite {
    pub network_name: String,
    pub network_secret: MeshSecret,
}

impl MeshInvite {
    pub fn encode(&self) -> String {
        self.encode_copy_code()
    }

    pub fn decode(value: &str) -> Result<Self, MeshConfigError> {
        decode_mesh_copy_code(value)
    }

    pub fn encode_copy_code(&self) -> String {
        let mut payload = Vec::new();
        payload.push(MESH_COPY_CODE_VERSION);
        push_u8_len_bytes(&mut payload, self.network_name.as_bytes());
        let (secret_kind, secret_bytes) =
            encode_copy_code_secret(self.network_secret.expose_secret());
        payload.push(secret_kind);
        push_u8_len_bytes(&mut payload, &secret_bytes);
        let checksum = fnv1a32(&payload);
        payload.extend_from_slice(&checksum.to_be_bytes());
        let encoded = crockford_base32_encode(&payload);
        format!(
            "{REMOTE_PLAY_DEVICE_GROUP_COPY_CODE_PREFIX}-{}",
            group_copy_code(&encoded)
        )
    }
}

const MESH_COPY_CODE_VERSION: u8 = 2;
const MESH_COPY_SECRET_UTF8: u8 = 0;
const MESH_COPY_SECRET_HEX_BYTES: u8 = 1;

fn decode_mesh_copy_code(value: &str) -> Result<MeshInvite, MeshConfigError> {
    let compact = compact_copy_code(value);
    if !compact
        .get(..REMOTE_PLAY_DEVICE_GROUP_COPY_CODE_PREFIX.len())
        .is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case(REMOTE_PLAY_DEVICE_GROUP_COPY_CODE_PREFIX)
        })
    {
        return Err(MeshConfigError::InvalidInviteCode);
    }
    let encoded = &compact[REMOTE_PLAY_DEVICE_GROUP_COPY_CODE_PREFIX.len()..];
    let payload = crockford_base32_decode(encoded)?;
    if payload.len() < 8 {
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
    if reader.read_u8()? != MESH_COPY_CODE_VERSION {
        return Err(MeshConfigError::InvalidInviteCode);
    }
    let network_name = String::from_utf8(reader.read_u8_len_bytes()?.to_vec())
        .map_err(|_| MeshConfigError::InvalidInviteCode)?;
    if network_name.trim().is_empty() {
        return Err(MeshConfigError::EmptyNetworkName);
    }
    let secret_kind = reader.read_u8()?;
    let secret = decode_copy_code_secret(secret_kind, reader.read_u8_len_bytes()?)?;
    if !reader.is_finished() {
        return Err(MeshConfigError::InvalidInviteCode);
    }
    Ok(MeshInvite {
        network_name,
        network_secret: MeshSecret::new(secret)?,
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

fn push_u8_len_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(bytes.len().try_into().unwrap_or(u8::MAX));
    out.extend_from_slice(&bytes[..bytes.len().min(u8::MAX as usize)]);
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
            out.push(ALPHABET[((buffer >> shift) & 0x1f) as usize] as char);
            bits -= 5;
            buffer &= (1 << bits) - 1;
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

fn crockford_base32_decode(value: &str) -> Result<Vec<u8>, MeshConfigError> {
    let mut out = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;
    for ch in value.chars() {
        let value = crockford_base32_value(ch).ok_or(MeshConfigError::InvalidInviteCode)?;
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
        let value = *self
            .bytes
            .get(self.pos)
            .ok_or(MeshConfigError::InvalidInviteCode)?;
        self.pos += 1;
        Ok(value)
    }
    fn read_u8_len_bytes(&mut self) -> Result<&'a [u8], MeshConfigError> {
        let len = self.read_u8()? as usize;
        let end = self
            .pos
            .checked_add(len)
            .ok_or(MeshConfigError::InvalidInviteCode)?;
        if end > self.bytes.len() {
            return Err(MeshConfigError::InvalidInviteCode);
        }
        let result = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(result)
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
    if let Some(path) = env::var_os(REMOTE_PLAY_DEVICE_GROUP_DIR_ENV)
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
            .join("NativeMesh");
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
            Ok(value) => MeshSecret::new(value.trim())
                .map(Some)
                .map_err(MeshStoreError::InvalidMeshConfig),
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
            self.save(&config)?; // normalize any pre-v2 metadata in place
            return Ok(config);
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
                f.write_str("stored device-group config is missing its secret")
            }
            Self::InvalidMetadata(message) => {
                write!(f, "stored device-group config is invalid: {message}")
            }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshConfigError {
    EmptyNetworkName,
    EmptyNetworkSecret,
    EmptyNodeId,
    EmptyDisplayName,
    InvalidInviteCode,
}
impl fmt::Display for MeshConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNetworkName => f.write_str("device-group network name is empty"),
            Self::EmptyNetworkSecret => f.write_str("device-group secret is empty"),
            Self::EmptyNodeId => f.write_str("device node id is empty"),
            Self::EmptyDisplayName => f.write_str("device display name is empty"),
            Self::InvalidInviteCode => f.write_str("device-group invite code is invalid"),
        }
    }
}
impl Error for MeshConfigError {}

const MESH_CONFIG_METADATA_VERSION: &str = "remote-play-device-group-v2";
const LEGACY_METADATA_VERSION: &str = "remote-play-mesh-v1";
const MESH_CONFIG_PLACEHOLDER_SECRET: &str = "metadata-secret-loaded-separately";

fn encode_mesh_config_metadata(config: &MeshConfig) -> String {
    format!(
        "version={MESH_CONFIG_METADATA_VERSION}\nnetwork_name_hex={}\nnode_id_hex={}\ndisplay_name_hex={}\n",
        hex_bytes(config.network_name.as_bytes()),
        hex_bytes(config.node_id.as_bytes()),
        hex_bytes(config.display_name.as_bytes()),
    )
}

fn decode_mesh_config_metadata(value: &str) -> Result<MeshConfig, MeshStoreError> {
    let mut version = None;
    let mut network_name = None;
    let mut node_id = None;
    let mut display_name = None;
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
            _ => {} // old metadata fields are ignored and removed on the next save
        }
    }
    if !matches!(
        version.as_deref(),
        Some(MESH_CONFIG_METADATA_VERSION | LEGACY_METADATA_VERSION)
    ) {
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
    })
}

fn required_metadata_string(
    key: &'static str,
    value: Option<String>,
) -> Result<String, MeshStoreError> {
    value.ok_or_else(|| MeshStoreError::InvalidMetadata(format!("missing {key}")))
}
fn decode_hex_string(value: &str) -> Result<String, MeshStoreError> {
    String::from_utf8(decode_hex_bytes(value)?)
        .map_err(|_| MeshStoreError::InvalidMetadata("hex value is not valid utf-8".to_string()))
}
fn decode_hex_bytes(value: &str) -> Result<Vec<u8>, MeshStoreError> {
    if !value.len().is_multiple_of(2) {
        return Err(MeshStoreError::InvalidMetadata(
            "hex value has odd length".to_string(),
        ));
    }
    let raw = value.as_bytes();
    let mut bytes = Vec::with_capacity(raw.len() / 2);
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
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_private_dir(parent)?;
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("device-group-store");
    let tmp_path = path.with_file_name(format!(".{file_name}.tmp-{}", hex_random::<4>()));
    let result = fs::write(&tmp_path, bytes)
        .map_err(|source| MeshStoreError::io(tmp_path.clone(), source))
        .and_then(|_| set_private_file_permissions(&tmp_path))
        .and_then(|_| {
            fs::rename(&tmp_path, path)
                .map_err(|source| MeshStoreError::io(path.to_path_buf(), source))
        })
        .and_then(|_| set_private_file_permissions(path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
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
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| MeshStoreError::io(path.to_path_buf(), source))
}
#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<(), MeshStoreError> {
    Ok(())
}
#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), MeshStoreError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|source| MeshStoreError::io(path.to_path_buf(), source))
}
#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), MeshStoreError> {
    Ok(())
}
fn non_empty_env_path(value: std::ffi::OsString) -> Option<std::ffi::OsString> {
    (!value.to_string_lossy().trim().is_empty()).then_some(value)
}
fn sanitized_display_name(display_name: impl Into<String>) -> String {
    let value = display_name.into();
    let value = value.trim();
    if value.is_empty() {
        "RemotePlay Device".to_string()
    } else {
        value.to_string()
    }
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
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_ID: AtomicU64 = AtomicU64::new(1);
    struct TempTree {
        root: PathBuf,
    }
    impl TempTree {
        fn new(name: &str) -> Self {
            let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let root =
                env::temp_dir().join(format!("remote-play-{name}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn generated_config_is_valid_and_redacts_secret() {
        let config = MeshConfig::generate("Alice Mac");
        config.validate().unwrap();
        assert!(config.network_name.starts_with("remote-play-"));
        assert_eq!(config.display_name, "Alice Mac");
        assert!(
            !format!("{:?}", config.network_secret).contains(config.network_secret.expose_secret())
        );
    }

    #[test]
    fn invite_roundtrips_group_credentials_without_node_identity() {
        let config = MeshConfig::generate("Host");
        let code = config.invite_code();
        assert!(code.starts_with(REMOTE_PLAY_DEVICE_GROUP_COPY_CODE_PREFIX));
        assert!(!code.contains(config.network_secret.expose_secret()));
        let joined = MeshConfig::from_invite_code(&code, "Viewer").unwrap();
        assert_eq!(joined.network_name, config.network_name);
        assert_eq!(
            joined.network_secret.expose_secret(),
            config.network_secret.expose_secret()
        );
        assert_ne!(joined.node_id, config.node_id);
        assert_eq!(joined.display_name, "Viewer");
    }

    #[test]
    fn invite_tolerates_case_whitespace_and_grouping() {
        let config = MeshConfig::generate("Host");
        let noisy = config
            .invite_code()
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
        let joined = MeshConfig::from_invite_code(&noisy, "Viewer").unwrap();
        assert_eq!(joined.network_name, config.network_name);
    }

    #[test]
    fn invite_rejects_checksum_changes() {
        let config = MeshConfig::generate("Host");
        let mut code = config.invite_code();
        let last = code.pop().unwrap();
        code.push(if last == 'A' { 'B' } else { 'A' });
        assert_eq!(
            MeshInvite::decode(&code).unwrap_err(),
            MeshConfigError::InvalidInviteCode
        );
    }

    #[test]
    fn store_roundtrips_config_and_secret() {
        let temp = TempTree::new("group-store");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        let config = MeshConfig::generate("Desk");
        let secret = config.network_secret.expose_secret().to_string();
        store.save(&config).unwrap();
        let metadata = fs::read_to_string(store.config_path()).unwrap();
        assert!(!metadata.contains(&secret));
        assert_eq!(store.load().unwrap().unwrap(), config);
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn store_normalizes_legacy_metadata_without_legacy_fields() {
        let temp = TempTree::new("group-migrate");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        let config = MeshConfig::generate("Desk");
        let metadata = format!(
            "version={LEGACY_METADATA_VERSION}\nnetwork_name_hex={}\nnode_id_hex={}\ndisplay_name_hex={}\nauto_start=1\nintegration_mode=old\ninitial_peer_hex=00\n",
            hex_bytes(config.network_name.as_bytes()),
            hex_bytes(config.node_id.as_bytes()),
            hex_bytes(config.display_name.as_bytes())
        );
        write_private_file(store.config_path(), metadata.as_bytes()).unwrap();
        store
            .secret_store()
            .save_secret(&config.network_secret)
            .unwrap();
        let loaded = store.load_or_generate("Ignored").unwrap();
        assert_eq!(loaded.network_name, config.network_name);
        let normalized = fs::read_to_string(store.config_path()).unwrap();
        assert!(normalized.contains(MESH_CONFIG_METADATA_VERSION));
        assert!(!normalized.contains("initial_peer"));
        assert!(!normalized.contains("integration_mode"));
    }

    #[cfg(unix)]
    #[test]
    fn store_uses_private_permissions() {
        let temp = TempTree::new("group-permissions");
        let store = AppPrivateMeshConfigStore::new(&temp.root);
        store.save(&MeshConfig::generate("Desk")).unwrap();
        assert_eq!(
            fs::metadata(store.root_dir()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.config_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(store.secret_store().path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
