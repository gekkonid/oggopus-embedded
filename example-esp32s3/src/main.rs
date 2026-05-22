#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use core::f64::consts::PI;

use esp_backtrace as _;
use esp_hal::main;
use esp_println::println;
use libm::{log10, sin};
use oggopus_embedded::{
    opus::{ChannelMapping, OpusHeader},
    prelude::*,
    states::Either,
};
use opus_embedded::prelude::*;

const SAMPLE_RATE: SamplingRate = SamplingRate::F48k;
const CHANNELS: Channels = Channels::Mono;
const FRAME_SIZE: FrameSize = FrameSize::Ms20;
const BITRATE: i32 = 48_000;
const PRE_SKIP: u16 = 312;
const TOTAL_FRAMES: usize = 100;
const TOTAL_SAMPLES: usize = 960 * TOTAL_FRAMES;

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let psram_config = esp_hal::psram::PsramConfig {
        mode: esp_hal::psram::PsramMode::OctalSpi,
        ..Default::default()
    };
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram, psram_config);
    println!("PSRAM allocator initialized");
    println!("Encode/decode roundtrip on ESP32-S3 starting");

    let fs = FRAME_SIZE.samples() as usize;

    // 1. Generate 440 Hz sine wave
    let mut pcm = Vec::with_capacity(TOTAL_SAMPLES);
    for i in 0..TOTAL_SAMPLES {
        let t = i as f64 / 48_000.0;
        let sample = (i16::MAX as f64 * sin(2.0 * PI * 440.0 * t)) as i16;
        pcm.push(sample);
    }
    println!("Generated {} PCM samples", pcm.len());

    // 2. Encode to Ogg Opus
    let mut encoder = Encoder::new(SAMPLE_RATE, CHANNELS, Application::Audio, FRAME_SIZE)
        .expect("failed to create encoder");
    encoder.set_bitrate(BITRATE).expect("failed to set bitrate");

    let mut writer = OggWriter::new(42, PRE_SKIP);
    let mut ogg = Vec::new();
    let mut page_buf = [0u8; 8192];

    // OpusHead
    let header = OpusHeader {
        version: 1,
        channels: ChannelMapping::Family0 { channels: 1 },
        pre_skip: PRE_SKIP,
        sample_rate: 48_000,
        output_gain: 0,
    };
    let n = writer.write_header(&header, &mut page_buf).unwrap();
    ogg.extend_from_slice(&page_buf[..n]);

    // OpusTags
    let n = writer
        .write_tags("rust-opus-embedded-esp32s3", &[], &mut page_buf)
        .unwrap();
    ogg.extend_from_slice(&page_buf[..n]);

    // Audio frames (same pattern as integration test: is_last=false on all audio packets)
    let mut enc_buf = [0u8; 2000];
    for frame in 0..TOTAL_FRAMES {
        let start = frame * fs;
        let packet = encoder
            .encode(&pcm[start..start + fs], &mut enc_buf)
            .expect("encoding failed");
        let n = writer
            .write_packet(packet, fs as u16, false, &mut page_buf)
            .unwrap();
        ogg.extend_from_slice(&page_buf[..n]);
    }

    // EOS page with padding packet (TOC 0xF8 = config 31, mono -> zero audio samples).
    // Must be a separate page so pre_skip granule lands correctly.
    let n = writer
        .write_packet(&[0xF8], 0, true, &mut page_buf)
        .unwrap();
    ogg.extend_from_slice(&page_buf[..n]);

    println!(
        "Encoded {} frames -> {} bytes in PSRAM",
        TOTAL_FRAMES,
        ogg.len()
    );

    // Free encoder and original PCM before decoding
    drop(encoder);
    drop(pcm);

    // 3. Decode from Ogg Opus
    let bitstream = Bitstream::new(&ogg);
    let (reader, header) = bitstream
        .reader()
        .read_header()
        .expect("failed to read header");
    let Either::Continued(mut reader) = reader else {
        panic!("stream ended after header");
    };
    let ch = match header.channels {
        ChannelMapping::Family0 { channels } => {
            Channels::try_from(channels).expect("invalid channel count")
        }
        _ => panic!("unsupported channel mapping"),
    };
    let sr =
        SamplingRate::try_from(header.sample_rate as i32).expect("unsupported sample rate");
    let mut decoder = Decoder::new(sr, ch).expect("failed to create decoder");

    let mut decoded = Vec::new();
    loop {
        let (new_reader, mut packets) = reader.next_packets::<1024>().unwrap();
        while let Some(packet) = packets.next() {
            let ns = decoder.get_nb_samples(packet.data).expect("invalid packet");
            let nch = ch as usize;
            let total = ns * nch;
            let offset = decoded.len();
            decoded.resize(offset + total, 0i16);
            decoder
                .decode(packet.data, &mut decoded[offset..])
                .expect("decoding failed");
        }
        match new_reader {
            Either::Ended(_) => break,
            Either::Continued(r) => reader = r,
        }
    }

    // Free ogg buffer before SNR computation
    drop(ogg);

    println!(
        "Decoded {} samples (raw, includes {} lookahead)",
        decoded.len(),
        PRE_SKIP,
    );

    // 4. Compute SNR (matching integration test logic)
    // Trim first 312 samples (libopus lookahead at 48 kHz); opusdec does this
    // implicitly in WAV output, but here we decode raw and must strip it.
    let decoded_trimmed = &decoded[PRE_SKIP as usize..];
    let min_len = TOTAL_SAMPLES.min(decoded_trimmed.len());
    let mut sq_err_sum = 0.0f64;
    let mut signal_power = 0.0f64;

    for i in 0..min_len {
        let t = i as f64 / 48_000.0;
        let original = (i16::MAX as f64 * sin(2.0 * PI * 440.0 * t)) as f64;
        let decoded_sample = decoded_trimmed[i] as f64;
        let err = original - decoded_sample;
        sq_err_sum += err * err;
        signal_power += original * original;
    }

    let mse = sq_err_sum / min_len as f64;
    let sp = signal_power / min_len as f64;
    let snr = if mse > 1e-30 && sp > 1e-30 {
        10.0 * log10(sp / mse)
    } else {
        100.0
    };

    println!("SNR: {:.2} dB ({} samples)", snr, min_len);

    if snr > 6.0 {
        println!("PASS: SNR ({:.2} dB) exceeds 6 dB threshold", snr);
    } else {
        println!("FAIL: SNR ({:.2} dB) below 6 dB threshold", snr);
    }

    loop {}
}
