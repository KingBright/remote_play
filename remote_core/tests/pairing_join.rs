//! Exercise the actual desktop pairing controller without linking the GPUI/media stack.
//! The controller uses only the same remote_core, Tokio and serde dependencies.
#[allow(dead_code)]
#[path = "../../client/src/mesh_pairing.rs"]
mod mesh_pairing;

#[cfg(feature = "pairing-image")]
#[test]
fn android_generated_image_joins_through_the_actual_controller_and_retains_identity() {
    use image::{DynamicImage, GrayImage, ImageFormat, Luma};
    use remote_core::mesh::AppPrivateMeshConfigStore;
    use std::{fs, io::Cursor};
    let mut lines = include_str!("fixtures/android-pairing-qr.txt").lines();
    let expected = remote_core::parse_pairing_qr(lines.next().unwrap()).unwrap();
    let rows: Vec<_> = lines.collect();
    assert!(rows.iter().all(|row| row.len() == rows.len()));
    let side = rows.len() as u32 * 6;
    let pixels = GrayImage::from_fn(side, side, |x, y| {
        Luma(
            [if rows[y as usize / 6].as_bytes()[x as usize / 6] == b'1' {
                0
            } else {
                255
            }],
        )
    });
    let mut png = Cursor::new(Vec::new());
    DynamicImage::ImageLuma8(pixels)
        .write_to(&mut png, ImageFormat::Png)
        .unwrap();
    let root = std::env::temp_dir().join(format!(
        "weavelo-android-qr-join-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let control =
        mesh_pairing::MeshPairingControl::load_or_create(&root, "Synthetic desktop").unwrap();
    let before = control.snapshot();
    let path = root.join("synthetic-phone.png");
    fs::write(&path, png.into_inner()).unwrap();
    let decoded = remote_core::pairing_image::decode_pairing_image_file(&path).unwrap();
    assert_eq!(decoded.invite_code, expected.invite_code);
    let joined = control.join_from_invite_code(&decoded.invite_code);
    assert_eq!(
        joined.message_kind,
        mesh_pairing::MeshPairingMessageKind::Success
    );
    assert_eq!(joined.device_id, before.device_id);
    assert_eq!(joined.display_name, before.display_name);
    let after = AppPrivateMeshConfigStore::new(&root)
        .load()
        .unwrap()
        .unwrap();
    let again = control.join_from_invite_code(&decoded.invite_code);
    assert_eq!(again.device_id, before.device_id);
    assert_eq!(
        AppPrivateMeshConfigStore::new(&root)
            .load()
            .unwrap()
            .unwrap(),
        after
    );
    fs::remove_dir_all(root).unwrap();
}
