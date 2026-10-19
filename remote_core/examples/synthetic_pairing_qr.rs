//! Reproducible QR interoperability fixture. No profile, networking or real credentials.
use remote_core::mesh::{MeshConfig, MeshSecret};
use remote_core::{encode_pairing_qr, qr_matrix_from_payload};

fn main() {
    let config = MeshConfig {
        network_name: "weavelo-qr-fixture".into(),
        network_secret: MeshSecret::new("synthetic-test-only-not-a-real-secret").unwrap(),
        node_id: "fixture-generator".into(),
        display_name: "Synthetic QR fixture".into(),
    };
    let payload = encode_pairing_qr(&config.invite_code(), 5154).unwrap();
    let matrix = qr_matrix_from_payload(&payload).unwrap();
    let mut fixture = format!("{payload}\n");
    for y in 0..matrix.width {
        fixture.extend((0..matrix.width).map(|x| if matrix.is_dark(x, y) { '1' } else { '0' }));
        fixture.push('\n');
    }
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("pairing-qr.txt"), fixture).unwrap();
    println!("Synthetic pairing QR fixture written (no device profile read).");
}
