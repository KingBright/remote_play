use super::*;
use crate::linux_frame::{Crop, SourceColor};
use std::time::Duration;

fn format(pixel: PixelFormat) -> FrameFormat {
    FrameFormat {
        width: 4,
        height: 2,
        pixel_format: pixel,
        color: SourceColor {
            range: 2,
            matrix: 0,
            transfer: 0,
            primaries: 0,
        },
        crop: None,
        transform: Transform::Identity,
    }
}
fn setup(generation: u64) -> (CopyContract, Arc<FrameMailbox>) {
    let mailbox = Arc::new(FrameMailbox::new(generation).unwrap());
    (CopyContract::new(generation, mailbox.clone()), mailbox)
}
fn chunk(bytes: &[u8], stride: i32) -> MappedChunk<'_> {
    MappedChunk {
        bytes,
        flags: 0,
        mapping_offset: 4096,
        offset: 0,
        size: bytes.len() as u32,
        stride,
    }
}

#[test]
fn neutral_or_corrupted_chunk_rejects_recycled_pixels_before_copy() {
    let (mut contract, mailbox) = setup(1);
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    // Structurally valid storage still contains pixels from its previous use.
    let recycled = [9; 12];
    assert_eq!(copy(&contract, &recycled).unwrap().planes[0], [9; 8]);
    for flags in [0b01, 0b10, 0b11] {
        let result = contract.copy(
            1,
            &[MappedChunk {
                flags,
                ..chunk(&recycled, 4)
            }],
            None,
            Transform::Identity,
            None,
            None,
            0,
        );
        assert!(matches!(result, Err(WorkerError::Buffer)));
        assert!(!mailbox.is_closed()); // The native caller owns stream teardown.
    }
}
fn copy(contract: &CopyContract, bytes: &[u8]) -> Result<OwnedFrame, WorkerError> {
    contract.copy(
        contract.revision(),
        &[chunk(bytes, 4)],
        None,
        Transform::Identity,
        Some(42),
        Some(-123),
        987,
    )
}

#[test]
fn contiguous_nv12_and_i420_copy_padding_without_aliasing_native_memory() {
    for pixel in [PixelFormat::Nv12, PixelFormat::I420] {
        let (mut contract, _) = setup(9);
        contract.renegotiate(Some(format(pixel))).unwrap();
        // Six-byte Y stride includes two padding bytes; single block chroma
        // is six-byte NV12 or three-byte I420 stride.
        let mut bytes = vec![
            1, 2, 3, 4, 99, 99, 5, 6, 7, 8, 99, 99, 9, 10, 11, 12, 99, 99,
        ];
        let owned = contract
            .copy(
                1,
                &[chunk(&bytes, 6)],
                None,
                Transform::Identity,
                Some(7),
                Some(-8),
                33,
            )
            .unwrap();
        bytes.fill(0); // PipeWire may reuse the buffer immediately after copy.
        assert_eq!(owned.planes[0], [1, 2, 3, 4, 5, 6, 7, 8]);
        match pixel {
            PixelFormat::Nv12 => assert_eq!(owned.planes[1], [9, 10, 11, 12]),
            PixelFormat::I420 => {
                assert_eq!(owned.planes[1], [9, 10]);
                assert_eq!(owned.planes[2], [12, 99]);
            }
            _ => unreachable!(),
        }
        assert_eq!(owned.stamp.format_revision, 1);
        assert_eq!(owned.stamp.pipewire_pts_ns, Some(-8));
        assert_eq!(owned.stamp.arrival_ts_us, 33);
        assert!(owned.mapping_offsets.iter().all(|offset| *offset == 4096));
    }
}

#[test]
fn separate_planes_keep_chunk_and_mapping_offsets_distinct_and_crop_pixels() {
    let (mut contract, _) = setup(1);
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    let y = [99, 1, 2, 3, 4, 5, 6, 7, 8, 99];
    let uv = [99, 9, 10, 11, 12, 99];
    let chunks = [
        MappedChunk {
            offset: 1,
            size: 8,
            ..chunk(&y, 4)
        },
        MappedChunk {
            offset: 1,
            size: 4,
            ..chunk(&uv, 4)
        },
    ];
    let owned = contract
        .copy(
            1,
            &chunks,
            Some(Crop {
                x: 2,
                y: 0,
                width: 2,
                height: 2,
            }),
            Transform::Identity,
            None,
            None,
            3,
        )
        .unwrap();
    assert_eq!(owned.planes, vec![vec![3, 4, 7, 8], vec![11, 12]]);
    assert_eq!((owned.width, owned.height), (2, 2));
}

#[test]
fn malformed_stride_chunk_plane_transform_and_unknown_range_fail_closed() {
    let (mut contract, _) = setup(1);
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    let bytes = [0; 12];
    for bad in [
        MappedChunk {
            stride: -4,
            ..chunk(&bytes, 4)
        },
        MappedChunk {
            stride: 3,
            ..chunk(&bytes, 4)
        },
        MappedChunk {
            offset: u32::MAX,
            ..chunk(&bytes, 4)
        },
        MappedChunk {
            size: 11,
            ..chunk(&bytes, 4)
        },
    ] {
        assert!(
            contract
                .copy(1, &[bad], None, Transform::Identity, None, None, 0)
                .is_err()
        );
    }
    assert!(
        contract
            .copy(1, &[], None, Transform::Identity, None, None, 0)
            .is_err()
    );
    assert!(
        contract
            .copy(
                1,
                &[chunk(&bytes, 4)],
                None,
                Transform::Unsupported(1),
                None,
                None,
                0
            )
            .is_err()
    );
    let mut unknown = format(PixelFormat::Nv12);
    unknown.color.range = 0;
    contract.renegotiate(Some(unknown)).unwrap();
    assert!(copy(&contract, &bytes).is_err());
    assert!(
        contract
            .renegotiate(Some(format(PixelFormat::Bgra)))
            .is_err()
    );
}

#[tokio::test]
async fn renegotiation_discards_queued_old_pixels_and_rejects_old_callback_ticket() {
    let (mut contract, mailbox) = setup(1);
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    let old = copy(&contract, &[1; 12]).unwrap();
    assert!(mailbox.publish(copy(&contract, &[2; 12]).unwrap()));
    contract.renegotiate(None).unwrap();
    assert!(!mailbox.publish(old));
    assert!(
        contract
            .copy(
                1,
                &[chunk(&[1; 12], 4)],
                None,
                Transform::Identity,
                None,
                None,
                0
            )
            .is_err()
    );
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    assert!(mailbox.publish(copy(&contract, &[3; 12]).unwrap()));
    let frame = mailbox.receive().await.unwrap();
    assert_eq!(frame.planes[0], [3; 8]);
    assert_eq!(frame.stamp.format_revision, 3);
}

#[tokio::test]
async fn latest_raw_queue_is_bounded_pause_resume_and_close_wake_waiter() {
    let (mut contract, mailbox) = setup(1);
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    for value in 1..100 {
        assert!(mailbox.publish(copy(&contract, &[value; 12]).unwrap()));
    }
    assert_eq!(mailbox.receive().await.unwrap().planes[0], [99; 8]);
    mailbox.set_paused(true);
    assert!(!mailbox.publish(copy(&contract, &[1; 12]).unwrap()));
    mailbox.set_paused(false);
    assert!(mailbox.publish(copy(&contract, &[2; 12]).unwrap()));
    assert_eq!(mailbox.receive().await.unwrap().planes[0], [2; 8]);
    let waiting = tokio::spawn({
        let mailbox = mailbox.clone();
        async move { mailbox.receive().await }
    });
    tokio::task::yield_now().await;
    mailbox.close();
    assert!(
        tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(copy(&contract, &[1; 12]).is_err());
}

#[tokio::test]
async fn expired_unsubmitted_frame_waits_for_a_fresh_frame_instead_of_resurfacing() {
    let mailbox = Arc::new(FrameMailbox::with_max_age(1, Some(Duration::from_millis(10))).unwrap());
    let mut contract = CopyContract::new(1, mailbox.clone());
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    mailbox.publish(copy(&contract, &[1; 12]).unwrap());
    tokio::time::sleep(Duration::from_millis(30)).await;
    let receive = mailbox.receive();
    tokio::pin!(receive);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), receive.as_mut())
            .await
            .is_err()
    );
    mailbox.publish(copy(&contract, &[2; 12]).unwrap());
    assert_eq!(receive.await.unwrap().planes[0], [2; 8]);
}

#[tokio::test]
async fn two_windows_have_independent_generation_format_and_lifetime() {
    let (mut a, ma) = setup(1);
    let (mut b, mb) = setup(2);
    a.renegotiate(Some(format(PixelFormat::Nv12))).unwrap();
    b.renegotiate(Some(format(PixelFormat::I420))).unwrap();
    assert!(!ma.publish(copy(&b, &[2; 12]).unwrap()));
    ma.close();
    assert!(copy(&a, &[1; 12]).is_err());
    assert!(mb.publish(copy(&b, &[2; 12]).unwrap()));
    assert_eq!(mb.receive().await.unwrap().stamp.generation, 2);
}

#[tokio::test]
async fn static_first_snapshot_is_delivered_once_without_renewing_age_then_expiry_applies() {
    let mailbox = Arc::new(FrameMailbox::native_stream(1, Duration::from_millis(10)).unwrap());
    let mut contract = CopyContract::new(1, mailbox.clone());
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    let mut old = copy(&contract, &[1; 12]).unwrap();
    old.received_at = std::time::Instant::now() - Duration::from_secs(1);
    mailbox.publish(old);
    let first = mailbox.receive().await.unwrap();
    assert!(first.initial_snapshot);
    assert!(first.is_expired(Duration::from_millis(10)));
    assert_eq!(first.stamp.arrival_ts_us, 987);
    let mut old = copy(&contract, &[2; 12]).unwrap();
    old.received_at = std::time::Instant::now() - Duration::from_secs(1);
    mailbox.publish(old);
    let receive = mailbox.receive();
    tokio::pin!(receive);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), receive.as_mut())
            .await
            .is_err()
    );
    mailbox.publish(copy(&contract, &[3; 12]).unwrap());
    let fresh = receive.await.unwrap();
    assert!(!fresh.initial_snapshot);
    assert_eq!(fresh.planes[0], [3; 8]);
}

#[test]
fn discontinuity_invalidates_old_raw_epoch_even_if_format_bytes_are_unchanged() {
    let (mut contract, mailbox) = setup(1);
    contract
        .renegotiate(Some(format(PixelFormat::Nv12)))
        .unwrap();
    let old = copy(&contract, &[1; 12]).unwrap();
    contract.discontinuity().unwrap();
    assert_eq!(contract.revision(), 2);
    assert!(!mailbox.publish(old));
    assert_eq!(copy(&contract, &[2; 12]).unwrap().stamp.format_revision, 2);
}

#[test]
fn final_chroma_row_can_omit_unused_stride_padding_without_overreading() {
    for pixel in [PixelFormat::Nv12, PixelFormat::I420] {
        let (mut contract, _) = setup(1);
        contract.renegotiate(Some(format(pixel))).unwrap();
        let bytes = [1, 2, 3, 4, 99, 99, 5, 6, 7, 8, 99, 99, 9, 10, 11, 12, 13];
        let needed = if pixel == PixelFormat::Nv12 { 16 } else { 17 };
        let frame = contract
            .copy(
                1,
                &[chunk(&bytes[..needed], 6)],
                None,
                Transform::Identity,
                None,
                None,
                0,
            )
            .unwrap();
        assert_eq!(frame.planes[0], [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(
            contract
                .copy(
                    1,
                    &[chunk(&bytes[..needed - 1], 6)],
                    None,
                    Transform::Identity,
                    None,
                    None,
                    0
                )
                .is_err()
        );
    }
}
