// Standalone, allocation-inclusive capture burst benchmark.
// CHUNKER_SOURCE=/absolute/audio.rs rustc -O audio-chunker-bench.rs -o chunker-bench
#[allow(dead_code)]
mod audio {
    include!(env!("CHUNKER_SOURCE"));
}

fn main() {
    for frames in [480usize, 960, 9600, 48000] {
        let input = vec![0.25f32; frames * 2];
        let mut chunker = audio::InterleavedAudioFrameChunker::new(48000, 2, 20).unwrap();
        let iterations = 5000;
        for _ in 0..100 {
            std::hint::black_box(chunker.push(&input));
        }
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(chunker.push(std::hint::black_box(&input)));
        }
        println!(
            "input_ms={} ns_per_call={:.1}",
            frames / 48,
            start.elapsed().as_nanos() as f64 / iterations as f64
        );
    }
}
