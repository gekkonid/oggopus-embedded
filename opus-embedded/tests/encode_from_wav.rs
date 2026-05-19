/*
 * Copyright (c) 2026 Tomi Leppänen
 * SPDX-License-Identifier: BSD-3-Clause
 *
 * Manual integration test: encode WAV files from tests/testdata/encode/
 * using both the embedded encoder and the external opusenc CLI, then
 * validate both outputs by decoding with opusdec.
 */

#![cfg(all(feature = "encode", feature = "decode"))]

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Command,
};

const WAV_DIR: &str = "../tests/testdata/encode";

fn wav_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(WAV_DIR)
}

fn find_wavs(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let entries = std::fs::read_dir(dir).expect("failed to read encode testdata directory");
    for entry in entries {
        let path = entry.expect("failed to read directory entry").path();
        if path.extension() == Some(OsStr::new("wav")) {
            result.push(path);
        }
    }
    result.sort();
    result
}

fn read_wav(path: &Path) -> (hound::WavSpec, Vec<i16>) {
    let mut reader = hound::WavReader::open(path).expect("failed to open WAV file");
    let spec = reader.spec();
    assert_eq!(
        spec.sample_format,
        hound::SampleFormat::Int,
        "expected integer PCM WAV"
    );
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .map(|s| s.expect("invalid sample in WAV"))
        .collect();
    (spec, samples)
}

fn encode_embedded(
    spec: &hound::WavSpec,
    samples: &[i16],
    output_path: &Path,
) {
    use oggopus_embedded::{
        opus::{ChannelMapping, OpusHeader},
        prelude::*,
    };
    use opus_embedded::prelude::*;

    let channels: Channels = match spec.channels {
        1 => Channels::Mono,
        2 => Channels::Stereo,
        n => panic!("unsupported channel count: {n}"),
    };
    let sample_rate: SamplingRate = SamplingRate::closest(spec.sample_rate as i32);
    let frame_size = FrameSize::Ms20;
    let fs = frame_size.samples();
    let pre_skip: u16 = 312;

    let mut encoder = Encoder::new(sample_rate, channels, Application::Audio, frame_size)
        .expect("failed to create encoder");
    encoder.set_bitrate(96000).expect("failed to set bitrate");

    let mut writer = OggWriter::new(42, pre_skip);

    let file = std::fs::File::create(output_path).expect("failed to create output file");
    let mut file = std::io::BufWriter::new(file);
    use std::io::Write;

    let mut page_buf = [0u8; 8192];

    let header = OpusHeader {
        version: 1,
        channels: match channels {
            Channels::Mono => ChannelMapping::Family0 { channels: 1 },
            Channels::Stereo => ChannelMapping::Family0 { channels: 2 },
        },
        pre_skip,
        sample_rate: spec.sample_rate,
        output_gain: 0,
    };
    let written = writer.write_header(&header, &mut page_buf).unwrap();
    file.write_all(&page_buf[..written]).unwrap();

    let written = writer
        .write_tags("rust-opus-embedded", &[], &mut page_buf)
        .unwrap();
    file.write_all(&page_buf[..written]).unwrap();

    let ch: i32 = channels.channels().into();
    let frame_samples = (fs * ch) as usize;
    let total_samples_per_ch = samples.len() / spec.channels as usize;
    let total_frames = (total_samples_per_ch + fs as usize - 1) / fs as usize;
    let last_frame_actual: u16 = if total_samples_per_ch % fs as usize == 0 {
        fs as u16
    } else {
        (total_samples_per_ch % fs as usize) as u16
    };

    let mut enc_buf = [0u8; 2000];
    for frame in 0..total_frames {
        let start = frame * frame_samples;
        let mut frame_input = vec![0i16; frame_samples];
        let copy_len = samples.len().saturating_sub(start).min(frame_samples);
        frame_input[..copy_len].copy_from_slice(&samples[start..start + copy_len]);

        let packet = encoder
            .encode(&frame_input, &mut enc_buf)
            .expect("encoding failed");
        let is_last = frame == total_frames - 1;
        let frame_n_samples = if is_last { last_frame_actual } else { fs as u16 };
        let written = writer
            .write_packet(packet, frame_n_samples, is_last, &mut page_buf)
            .unwrap();
        file.write_all(&page_buf[..written]).unwrap();
    }

    file.flush().unwrap();
}

fn encode_opusenc(input: &Path, output: &Path) {
    let cmd_output = Command::new("opusenc")
        .arg("--quiet")
        .arg(input)
        .arg(output)
        .output()
        .expect("opusenc not found -- install opus-tools");
    assert!(
        cmd_output.status.success(),
        "opusenc failed:\nstderr: {}",
        String::from_utf8_lossy(&cmd_output.stderr),
    );
}

fn decode_with_opusdec(opus_path: &Path) -> Vec<i16> {
    let tmp = std::env::temp_dir();
    let stem = opus_path.file_stem().unwrap().to_string_lossy();
    let wav_path = tmp.join(format!("{}.wav", stem));

    let output = Command::new("opusdec")
        .arg("--quiet")
        .arg(opus_path)
        .arg(&wav_path)
        .output()
        .expect("opusdec not found -- install opus-tools");
    assert!(
        output.status.success(),
        "opusdec failed:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let mut reader =
        hound::WavReader::open(&wav_path).expect("failed to open WAV output from opusdec");
    let spec = reader.spec();
    assert_eq!(spec.sample_format, hound::SampleFormat::Int);
    assert_eq!(spec.bits_per_sample, 16);
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .map(|s| s.expect("invalid sample in WAV"))
        .collect();

    let _ = std::fs::remove_file(&wav_path);
    samples
}

fn check_opusinfo(path: &Path) {
    let output = Command::new("opusinfo")
        .arg(path)
        .output()
        .expect("opusinfo not found -- install opus-tools");
    assert!(
        output.status.success(),
        "opusinfo failed on {}:\nstdout: {}\nstderr: {}",
        path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        panic!(
            "opusinfo reported warnings for {}:\n{}",
            path.display(),
            stderr
        );
    }
}

#[test]
fn encode_wav_files_with_embedded_and_opusenc() {
    let dir = wav_dir();
    assert!(
        dir.exists(),
        "encode testdata directory not found: {}",
        dir.display()
    );

    let wavs = find_wavs(&dir);
    assert!(!wavs.is_empty(), "no .wav files found in {}", dir.display());

    for wav_path in &wavs {
        let stem = wav_path.file_stem().unwrap().to_string_lossy();
        let embedded_path = dir.join(format!("{}.embedded.opus", stem));
        let opusenc_path = dir.join(format!("{}.opusenc.opus", stem));

        let (spec, samples) = read_wav(wav_path);

        let duration = samples.len() as f64 / spec.sample_rate as f64 / spec.channels as f64;
        println!(
            "Encoding {} ({} ch, {} Hz, {:.1} s) with embedded encoder -> {}",
            wav_path.display(),
            spec.channels,
            spec.sample_rate,
            duration,
            embedded_path.display(),
        );
        encode_embedded(&spec, &samples, &embedded_path);

        println!(
            "Encoding {} with opusenc -> {}",
            wav_path.display(),
            opusenc_path.display()
        );
        encode_opusenc(wav_path, &opusenc_path);

        println!("Checking {} with opusinfo", embedded_path.display());
        check_opusinfo(&embedded_path);

        println!("Checking {} with opusinfo", opusenc_path.display());
        check_opusinfo(&opusenc_path);

        let decoded_embedded = decode_with_opusdec(&embedded_path);
        let decoded_opusenc = decode_with_opusdec(&opusenc_path);

        assert!(!decoded_embedded.is_empty(), "embedded {} decoded no samples", stem);
        assert!(!decoded_opusenc.is_empty(), "opusenc {} decoded no samples", stem);

        let peak_embedded = decoded_embedded.iter().map(|&s| s.abs()).max().unwrap_or(0);
        let peak_opusenc = decoded_opusenc.iter().map(|&s| s.abs()).max().unwrap_or(0);

        println!(
            "{}: embedded {} samples (peak={}), opusenc {} samples (peak={})",
            stem,
            decoded_embedded.len(),
            peak_embedded,
            decoded_opusenc.len(),
            peak_opusenc,
        );

        assert!(peak_embedded > 0, "embedded {} output is all zeros", stem);
        assert!(peak_opusenc > 0, "opusenc {} output is all zeros", stem);

        println!("OK: {} (results in {})", stem, dir.display());
    }
}
