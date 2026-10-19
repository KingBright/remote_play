//! Local image decoding only. Credentials still pass through the existing invitation validator.
use crate::{
    mesh::MeshInvite,
    pairing_qr::{PairingQrPayload, parse_pairing_qr},
};
use std::{
    fmt,
    fs::OpenOptions,
    io::{Cursor, Read},
    path::Path,
};

const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingImageError {
    Read,
    Unsupported,
    TooLarge,
    NoInvitation,
    MultipleInvitations,
}
impl fmt::Display for PairingImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "Could not read the selected image.",
            Self::Unsupported => "Choose a readable PNG, JPEG or WebP image.",
            Self::TooLarge => "The image is too large. Crop it to the invitation QR and try again.",
            Self::NoInvitation => "No valid Weavelo invitation was found in this image.",
            Self::MultipleInvitations => {
                "This image contains different invitations. Crop it to one QR code."
            }
        })
    }
}
impl std::error::Error for PairingImageError {}

pub fn decode_pairing_image_file(path: &Path) -> Result<PairingQrPayload, PairingImageError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A selected symlink/FIFO must not block the decoder before type checks.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| PairingImageError::Read)?;
    let metadata = file.metadata().map_err(|_| PairingImageError::Read)?;
    if !metadata.is_file() {
        return Err(PairingImageError::Read);
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(PairingImageError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PairingImageError::Read)?;
    decode_pairing_image(&bytes)
}

pub fn decode_pairing_image(bytes: &[u8]) -> Result<PairingQrPayload, PairingImageError> {
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(PairingImageError::TooLarge);
    }
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| PairingImageError::Unsupported)?;
    let format = reader.format().ok_or(PairingImageError::Unsupported)?;
    if !matches!(
        format,
        image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::WebP
    ) {
        return Err(PairingImageError::Unsupported);
    }
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| PairingImageError::Unsupported)?;
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(PairingImageError::TooLarge);
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let rgba = reader
        .decode()
        .map_err(|_| PairingImageError::Unsupported)?
        .into_rgba8();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width as usize, height as usize, |x, y| {
            let p = rgba.get_pixel(x as u32, y as u32).0;
            let luminance =
                (u32::from(p[0]) * 299 + u32::from(p[1]) * 587 + u32::from(p[2]) * 114) / 1000;
            ((luminance * u32::from(p[3]) + 255 * (255 - u32::from(p[3]))) / 255) as u8
        });
    let mut invitation: Option<PairingQrPayload> = None;
    for grid in prepared.detect_grids() {
        let Ok((_, raw)) = grid.decode() else {
            continue;
        };
        let Ok(mut payload) = parse_pairing_qr(&raw) else {
            continue;
        };
        let Ok(decoded) = MeshInvite::decode(&payload.invite_code) else {
            continue;
        };
        payload.invite_code = decoded.encode_copy_code();
        if let Some(existing) = &invitation {
            if existing.invite_code != payload.invite_code {
                return Err(PairingImageError::MultipleInvitations);
            }
        } else {
            invitation = Some(payload);
        }
    }
    invitation.ok_or(PairingImageError::NoInvitation)
}

/// One in-flight picker/decoder per view. Cancelled work must finish before another starts.
#[derive(Default)]
pub struct PairingImageImport {
    next: u64,
    pending: Option<u64>,
    cancelled: bool,
}
impl PairingImageImport {
    pub fn begin(&mut self) -> Option<u64> {
        if self.pending.is_some() {
            return None;
        }
        self.next = self.next.wrapping_add(1);
        self.pending = Some(self.next);
        self.cancelled = false;
        self.pending
    }
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
    pub fn finish(&mut self, ticket: u64) -> bool {
        if self.pending != Some(ticket) {
            return false;
        }
        self.pending = None;
        !self.cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode_pairing_qr, mesh::MeshConfig, qr_matrix_from_payload};
    use image::{DynamicImage, GrayImage, Luma};

    fn qr_image(payload: &str) -> GrayImage {
        let matrix = qr_matrix_from_payload(payload).unwrap();
        let side = (matrix.width + 8) * 4;
        GrayImage::from_fn(side as u32, side as u32, |x, y| {
            let x = x as usize / 4;
            let y = y as usize / 4;
            Luma([
                if x >= 4
                    && y >= 4
                    && x < matrix.width + 4
                    && y < matrix.width + 4
                    && matrix.is_dark(x - 4, y - 4)
                {
                    0
                } else {
                    255
                },
            ])
        })
    }
    fn bytes(image: GrayImage, format: image::ImageFormat) -> Vec<u8> {
        let mut data = Cursor::new(Vec::new());
        DynamicImage::ImageLuma8(image)
            .write_to(&mut data, format)
            .unwrap();
        data.into_inner()
    }
    #[test]
    fn reads_png_jpeg_webp_and_rotated_synthetic_invitations() {
        let config = MeshConfig::generate("Synthetic phone");
        let payload = encode_pairing_qr(&config.invite_code(), 5154).unwrap();
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::WebP,
        ] {
            let image = image::imageops::rotate90(&qr_image(&payload));
            assert_eq!(
                decode_pairing_image(&bytes(image, format))
                    .unwrap()
                    .invite_code,
                config.invite_code()
            );
        }
    }
    #[test]
    fn repeated_same_invitation_is_unambiguous_but_two_groups_are_rejected() {
        let first = MeshConfig::generate("Synthetic first");
        let second = MeshConfig::generate("Synthetic second");
        let left = qr_image(&encode_pairing_qr(&first.invite_code(), 5154).unwrap());
        for (right, expected_error) in [(&first, false), (&second, true)] {
            let right = qr_image(&encode_pairing_qr(&right.invite_code(), 5154).unwrap());
            let mut both = GrayImage::from_pixel(
                left.width() + right.width() + 30,
                left.height().max(right.height()),
                Luma([255]),
            );
            image::imageops::overlay(&mut both, &left, 0, 0);
            image::imageops::overlay(&mut both, &right, i64::from(left.width() + 30), 0);
            let decoded = decode_pairing_image(&bytes(both, image::ImageFormat::Png));
            if expected_error {
                assert_eq!(decoded.unwrap_err(), PairingImageError::MultipleInvitations);
            } else {
                assert_eq!(decoded.unwrap().invite_code, first.invite_code());
            }
        }
    }
    #[test]
    fn invalid_oversized_and_non_invitation_images_fail_without_leaking_text() {
        assert_eq!(
            decode_pairing_image(b"broken PNG").unwrap_err(),
            PairingImageError::Unsupported
        );
        assert_eq!(
            decode_pairing_image(&vec![0; MAX_FILE_BYTES as usize + 1]).unwrap_err(),
            PairingImageError::TooLarge
        );
        let too_wide = GrayImage::new(MAX_DIMENSION + 1, 1);
        assert_eq!(
            decode_pairing_image(&bytes(too_wide, image::ImageFormat::Png)).unwrap_err(),
            PairingImageError::TooLarge
        );
        for raw in ["https://example.invalid/", "weavelo://join?code=invalid"] {
            assert_eq!(
                decode_pairing_image(&bytes(qr_image(raw), image::ImageFormat::Png)).unwrap_err(),
                PairingImageError::NoInvitation
            );
        }
    }
    #[test]
    fn cancellation_repetition_and_late_results_cannot_join() {
        let mut state = PairingImageImport::default();
        let cancelled = state.begin().unwrap();
        assert!(state.begin().is_none());
        state.cancel();
        assert!(state.begin().is_none());
        assert!(!state.finish(cancelled));
        let current = state.begin().unwrap();
        assert!(!state.finish(cancelled));
        assert!(state.is_pending());
        assert!(state.finish(current));
        assert!(!state.finish(current));
    }
}
