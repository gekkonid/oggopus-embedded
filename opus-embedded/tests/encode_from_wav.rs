/*
 * Copyright (c) 2026 Gekkonid Scientific
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

const PRE_SKIP: u16 = 312;
const TARGET_BITRATE: u32 = 48_000;

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
    encoder.set_bitrate(TARGET_BITRATE as i32).expect("failed to set bitrate");

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
    let bitrate = TARGET_BITRATE / 1000;
    let cmd_output = Command::new("opusenc")
        .arg("--quiet")
        .arg("--bitrate")
        .arg(bitrate.to_string())
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

fn run_opusinfo(path: &Path) -> String {
    let output = Command::new("opusinfo")
        .arg(path)
        .output()
        .expect("opusinfo not found -- install opus-tools");

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        panic!(
            "opusinfo reported warnings for {}:\n{}",
            path.display(),
            stderr
        );
    }
    assert!(
        output.status.success(),
        "opusinfo failed on {}: {}",
        path.display(),
        stderr
    );
    String::from_utf8(output.stdout).expect("opusinfo output is not valid UTF-8")
}

fn parse_opusinfo_field<'a>(lines: &[&'a str], prefix: &str) -> Option<&'a str> {
    for &line in lines {
        if let Some(value) = line.trim().strip_prefix(prefix) {
            return Some(value.trim());
        }
    }
    None
}

fn parse_duration(s: &str) -> f64 {
    let s = s.trim().trim_end_matches('s');
    if let Some((min, rest)) = s.split_once("m:") {
        let minutes: f64 = min.parse().unwrap_or(0.0);
        let seconds: f64 = rest.parse().unwrap_or(0.0);
        minutes * 60.0 + seconds
    } else {
        s.parse::<f64>().unwrap_or(0.0)
    }
}

fn parse_bitrate(s: &str) -> f64 {
    let s = s.trim();
    // "107.6 kbit/s" or "96.41 kbit/s"
    s.split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn validate_opusinfo_output(
    stdout: &str,
    stem: &str,
    label: &str,
    expected_channels: u16,
    expected_sample_rate: u32,
    expected_duration: f64,
    expect_embedded_encoder: bool,
) {
    let lines: Vec<&str> = stdout.lines().collect();

    if expect_embedded_encoder {
        let encoder =
            parse_opusinfo_field(&lines, "Encoded with ").expect("missing encoder field");
        assert_eq!(
            encoder, "rust-opus-embedded",
            "{} {}: unexpected encoder",
            label, stem
        );
    } else {
        let encoder =
            parse_opusinfo_field(&lines, "Encoded with ").expect("missing encoder field");
        assert!(
            encoder.contains("libopus"),
            "{} {}: expected libopus encoder, got '{}'",
            label,
            stem,
            encoder
        );
    }

    let pre_skip_str = parse_opusinfo_field(&lines, "Pre-skip: ").expect("missing pre-skip");
    let pre_skip: u16 = pre_skip_str.parse().expect("invalid pre-skip value");
    assert_eq!(
        pre_skip, PRE_SKIP,
        "{} {}: expected pre-skip {}, got {}",
        label, stem, PRE_SKIP, pre_skip
    );

    let channels_str = parse_opusinfo_field(&lines, "Channels: ").expect("missing channels");
    let channels: u16 = channels_str.parse().expect("invalid channels value");
    assert_eq!(
        channels, expected_channels,
        "{} {}: expected {} channels, got {}",
        label, stem, expected_channels, channels
    );

    let rate_str =
        parse_opusinfo_field(&lines, "Original sample rate: ").expect("missing sample rate");
    let rate_str = rate_str.trim_end_matches(" Hz");
    let rate: u32 = rate_str.parse().expect("invalid sample rate");
    assert_eq!(
        rate, expected_sample_rate,
        "{} {}: expected sample rate {}, got {}",
        label, stem, expected_sample_rate, rate
    );

    let packet_dur = parse_opusinfo_field(&lines, "Packet duration: ").expect("missing packet duration");
    assert!(
        packet_dur.contains("20.0ms"),
        "{} {}: expected 20.0ms packet duration, got '{}'",
        label,
        stem,
        packet_dur
    );

    let playback = parse_opusinfo_field(&lines, "Playback length: ").expect("missing playback length");
    let playback_secs = parse_duration(playback);
    let duration_diff = (playback_secs - expected_duration).abs();
    assert!(
        duration_diff < 0.01,
        "{} {}: playback length {:.3}s differs from expected {:.3}s by {:.3}s",
        label,
        stem,
        playback_secs,
        expected_duration,
        duration_diff
    );

    let bitrate_line =
        parse_opusinfo_field(&lines, "Average bitrate: ").expect("missing bitrate");
    if let Some(w_o) = bitrate_line.split(',').nth(1) {
        if let Some(rate_str) = w_o.trim().strip_prefix("w/o overhead: ") {
            let rate_kbps = parse_bitrate(rate_str);
            let expected_kbps = TARGET_BITRATE as f64 / 1000.0;
            let bitrate_ratio = rate_kbps / expected_kbps;
            assert!(
                (0.8..=1.2).contains(&bitrate_ratio),
                "{} {}: bitrate w/o overhead {:.1} kbps too far from target {} kbps",
                label,
                stem,
                rate_kbps,
                expected_kbps
            );
        }
    }
}

fn compute_snr(original: &[i16], decoded: &[i16]) -> f64 {
    let min_len = original.len().min(decoded.len());
    if min_len == 0 {
        return -f64::INFINITY;
    }
    let mut sq_err_sum: f64 = 0.0;
    let mut signal_power: f64 = 0.0;
    for i in 0..min_len {
        let err = (original[i] as f64) - (decoded[i] as f64);
        sq_err_sum += err * err;
        signal_power += (original[i] as f64) * (original[i] as f64);
    }
    let mse = sq_err_sum / min_len as f64;
    let sp = signal_power / min_len as f64;
    if mse <= 1e-30 || sp <= 1e-30 {
        return 100.0;
    }
    10.0 * (sp / mse).log10()
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
        let is_direct_rate = opus_embedded::SamplingRate::closest(spec.sample_rate as i32) as u32
            == spec.sample_rate;

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
        let embedded_info = run_opusinfo(&embedded_path);
        validate_opusinfo_output(
            &embedded_info,
            &stem,
            "embedded",
            spec.channels,
            spec.sample_rate,
            duration,
            true,
        );

        println!("Checking {} with opusinfo", opusenc_path.display());
        let opusenc_info = run_opusinfo(&opusenc_path);
        validate_opusinfo_output(
            &opusenc_info,
            &stem,
            "opusenc",
            spec.channels,
            spec.sample_rate,
            duration,
            false,
        );

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

        if is_direct_rate {
            let min_len = samples.len().min(decoded_embedded.len().min(decoded_opusenc.len()));
            if min_len > 0 {
                let aligned = &samples[..min_len];
                let snr_embedded =
                    compute_snr(aligned, &decoded_embedded[..min_len]);
                let snr_opusenc =
                    compute_snr(aligned, &decoded_opusenc[..min_len]);

                let sample_diff_embedded =
                    decoded_embedded.len() as isize - samples.len() as isize;
                let sample_diff_opusenc =
                    decoded_opusenc.len() as isize - samples.len() as isize;

                println!(
                    "{}: embedded {} samples ({} vs source), opusenc {} samples ({} vs source)",
                    stem,
                    decoded_embedded.len(),
                    if sample_diff_embedded >= 0 { format!("+{sample_diff_embedded}") } else { format!("{sample_diff_embedded}") },
                    decoded_opusenc.len(),
                    if sample_diff_opusenc >= 0 { format!("+{sample_diff_opusenc}") } else { format!("{sample_diff_opusenc}") },
                );
                println!(
                    "{}: embedded SNR={:.1} dB, opusenc SNR={:.1} dB ({} samples)",
                    stem, snr_embedded, snr_opusenc, min_len
                );

                assert!(
                    snr_embedded > 6.0,
                    "{}: embedded SNR too low: {:.1} dB",
                    stem,
                    snr_embedded
                );
                assert!(
                    snr_opusenc > 6.0,
                    "{}: opusenc SNR too low: {:.1} dB",
                    stem,
                    snr_opusenc
                );
            }
        } else {
            println!(
                "{}: sample rate {} Hz differs from Opus rate, skipping SNR check",
                stem,
                spec.sample_rate
            );
        }

        println!("OK: {} (results in {})", stem, dir.display());
    }
}
