/*
 * Copyright (c) 2025 Tomi Leppänen
 * SPDX-License-Identifier: BSD-3-Clause
 */
/*!
 * Small no_std and no_alloc opus decoder and encoder.
 *
 * Uses libopus.
 */

#![no_std]
#![deny(missing_docs)]

use az::SaturatingAs;
use core::ffi::{c_int, CStr};
use num_enum::{IntoPrimitive, TryFromPrimitive};
use opus_embedded_sys::*;

pub mod prelude {
    /*!
     * opus_embedded prelude.
     *
     * Includes the most commonly needed types.
     *
     * ```
     * # #![allow(unused_imports)]
     * use opus_embedded::prelude::*;
     * ```
     */

    #[cfg(feature = "decode")]
    pub use super::Decoder;

    #[cfg(feature = "encode")]
    pub use super::{Application, Encoder};

    pub use super::{Channels, SamplingRate};
}

/**
 * # Safety
 *
 * The implementation of numeric must return a valid error code defined by libopus.
 */
unsafe trait RawOpusError {
    /**
     * Returns valid numeric error code defined by libopus.
     */
    fn numeric(&self) -> c_int;
}

/// Error from parsing opus data.
pub trait OpusError {
    /// Returns the error message as it is defined by libopus.
    fn message(&self) -> &'static str;
}

impl<E: RawOpusError> OpusError for E {
    fn message(&self) -> &'static str {
        // SAFETY: OpusError::numeric() returns valid error code and null value is handled
        let error = unsafe {
            let error = opus_strerror(self.numeric());
            if error.is_null() {
                return "Unknown error";
            }
            CStr::from_ptr(error)
        };
        error.to_str().unwrap()
    }
}

#[cfg(feature = "decode")]
mod decode {
    use super::*;

    /// Error from decoding opus data.
    #[derive(Debug, PartialEq)]
    pub struct DecoderError {
        pub(crate) error_code: c_int,
    }

    unsafe impl RawOpusError for DecoderError {
        fn numeric(&self) -> c_int {
            // SAFETY: This error code was given by libopus and we trust that it is correct
            self.error_code
        }
    }

    impl core::fmt::Display for DecoderError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(self.message())
        }
    }

    impl core::error::Error for DecoderError {
        fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
            None
        }
    }

    /// Invalid opus data packet encountered.
    #[derive(Debug, PartialEq)]
    pub struct InvalidPacket {}

    unsafe impl RawOpusError for InvalidPacket {
        fn numeric(&self) -> c_int {
            OPUS_INVALID_PACKET
        }
    }

    impl core::fmt::Display for InvalidPacket {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(self.message())
        }
    }

    impl core::error::Error for InvalidPacket {
        fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
            None
        }
    }

    /// Opus decoder.
    #[derive(Debug)]
    pub struct Decoder {
        pub(crate) decoder: OpusDecoder,
        pub(crate) channels: Channels,
    }

    impl Decoder {
        /**
         * Construct decoder from requested sampling rate and number of channels.
         *
         * See also [`opus_decoder_get_size`] and [`opus_decoder_init`].
         */
        pub fn new(freq: SamplingRate, channels: Channels) -> Result<Self, DecoderError> {
            if !cfg!(feature = "stereo") && channels == Channels::Stereo {
                let error_code = OPUS_ALLOC_FAIL;
                return Err(DecoderError { error_code });
            }
            let mut decoder = Decoder {
                decoder: OpusDecoder::default(),
                channels,
            };
            let channels = channels.channels().into();
            // SAFETY: The number of channels can be only one or two as required
            let size = unsafe { opus_decoder_get_size(channels) };
            assert!(
                core::mem::size_of::<OpusDecoder>() >= size.try_into().unwrap(),
                "OpusDecoder struct is too small!"
            );
            // SAFETY: decoder.decoder points to a correct sized chunk of memory
            let error_code =
                unsafe { opus_decoder_init(&mut decoder.decoder, freq.into(), channels) };
            // PANIC: All error codes are small integers
            if error_code != OPUS_OK.try_into().unwrap() {
                Err(DecoderError { error_code })
            } else {
                Ok(decoder)
            }
        }

        /**
         * Return the number of samples in the opus data multiplied by the number of channels.
         *
         * This value can be used for output buffer size for decoding when total number of samples
         * in frame is expected.
         *
         * See also [`Decoder::get_nb_samples`].
         */
        pub fn get_nb_samples_total(&self, data: &[u8]) -> Result<usize, DecoderError> {
            match self.channels {
                Channels::Mono => self.get_nb_samples(data),
                Channels::Stereo => Ok(self.get_nb_samples(data)? * 2),
            }
        }

        /**
         * Return the number of samples in the opus data.
         *
         * This value can be used for audio output when frame size is expected, i.e. the number of
         * samples per channel.
         *
         * See also [`opus_decoder_get_nb_samples`].
         */
        pub fn get_nb_samples(&self, data: &[u8]) -> Result<usize, DecoderError> {
            // SAFETY: The pointer points to a valid slice of data or null if the slice was empty.
            // Length is derived from the input slice
            let samples = unsafe {
                let len = data.len().saturating_as();
                let data = if !data.is_empty() {
                    data.as_ptr()
                } else {
                    core::ptr::null()
                };
                opus_decoder_get_nb_samples(&self.decoder, data, len)
            };
            if samples < 0 {
                Err(DecoderError {
                    error_code: samples,
                })
            } else {
                Ok(samples.saturating_as())
            }
        }

        /**
         * Decode opus packet from data into output buffer.
         *
         * Returns decoded frame stored on output buffer. Its length is total number of samples in a
         * frame.
         *
         * See also [`opus_decode`].
         */
        pub fn decode<'output>(
            &mut self,
            data: &[u8],
            output: &'output mut [i16],
        ) -> Result<&'output [i16], DecoderError> {
            // SAFETY: The pointers point to valid slices of data or null if their respective slice
            // was empty. Lengths are derived from the respective slices
            let samples = unsafe {
                let len: i32 = data.len().saturating_as();
                let data = if !data.is_empty() {
                    data.as_ptr()
                } else {
                    core::ptr::null()
                };
                // Let's calculate frame_size that will fit in the output buffer
                let frame_size: i32 = match self.channels {
                    Channels::Mono => output.len(),
                    Channels::Stereo => output.len() / 2,
                }
                .saturating_as();
                let output = if !output.is_empty() {
                    output.as_mut_ptr()
                } else {
                    core::ptr::null_mut()
                };
                opus_decode(&mut self.decoder, data, len, output, frame_size, 0)
            };
            if samples < 0 {
                Err(DecoderError {
                    error_code: samples,
                })
            } else {
                let frame_size = match self.channels {
                    Channels::Mono => samples as usize,
                    Channels::Stereo => samples as usize * 2,
                };
                Ok(&output[..frame_size])
            }
        }
    }

    /// Bandwidth in the opus data.
    #[derive(Copy, Clone, Debug, PartialEq)]
    pub enum Bandwidth {
        /// Narrowband data (4 kHz bandpass).
        Narrowband,
        /// Mediumband data (6 kHz bandpass).
        Mediumband,
        /// Wideband data (8 kHz bandpass).
        Wideband,
        /// Superwideband data (12 kHz bandpass).
        Superwideband,
        /// Fullband data (20 kHz bandpass).
        Fullband,
    }

    /// Wraps opus data into a packet type.
    #[derive(Debug)]
    pub struct OpusPacket<'data> {
        data: &'data [u8],
    }

    impl<'data> OpusPacket<'data> {
        /**
         * Construct packet from data.
         *
         * Does not check for validity.
         *
         * See also [`opus_packet_get_nb_channels`].
         *
         * # Panics
         * Panics if data is an empty slice.
         */
        pub fn new(data: &'data [u8]) -> Self {
            assert!(!data.is_empty());
            Self { data }
        }

        /// Return the number of channels for the packet.
        pub fn get_nb_channels(&self) -> Result<u8, InvalidPacket> {
            // SAFETY: The pointer points to a valid slice of data, and the size is not zero
            let channels = unsafe {
                let data = self.data.as_ptr();
                opus_packet_get_nb_channels(data)
            };
            if channels < 0 {
                debug_assert_eq!(channels, OPUS_INVALID_PACKET);
                Err(InvalidPacket {})
            } else {
                Ok(channels.saturating_as())
            }
        }

        /**
         * Return the number of frames for the packet.
         *
         * See also [`opus_packet_get_nb_frames`].
         */
        pub fn get_nb_frames(&self) -> Result<u32, InvalidPacket> {
            // SAFETY: The pointer points to a valid slice of data, the length is derived from the
            // slice and the slice is not empty
            let frames = unsafe {
                let len = self.data.len().saturating_as();
                let data = self.data.as_ptr();
                opus_packet_get_nb_frames(data, len)
            };
            if frames < 0 {
                debug_assert_eq!(frames, OPUS_INVALID_PACKET);
                Err(InvalidPacket {})
            } else {
                Ok(frames.saturating_as())
            }
        }

        /**
         * Return the bandwidth of the packet.
         *
         * See also [`opus_packet_get_bandwidth`].
         */
        pub fn get_bandwidth(&self) -> Result<Bandwidth, InvalidPacket> {
            // SAFETY: The pointer points to a valid slice of data, and the size is not zero
            let bandwidth = unsafe {
                let data = self.data.as_ptr();
                opus_packet_get_bandwidth(data)
            };
            if bandwidth < 0 {
                debug_assert_eq!(bandwidth, OPUS_INVALID_PACKET);
                Err(InvalidPacket {})
            } else {
                use Bandwidth::*;
                // PANIC: All bandwidth values are small positive integers
                #[allow(non_snake_case)]
                Ok(match bandwidth.try_into().unwrap() {
                    OPUS_BANDWIDTH_NARROWBAND => Narrowband,
                    OPUS_BANDWIDTH_MEDIUMBAND => Mediumband,
                    OPUS_BANDWIDTH_WIDEBAND => Wideband,
                    OPUS_BANDWIDTH_SUPERWIDEBAND => Superwideband,
                    OPUS_BANDWIDTH_FULLBAND => Fullband,
                    _ => panic!("Invalid bandwidth value returned by libopus"),
                })
            }
        }

        /**
         * Return the number of sampels per frame in the packet.
         *
         * See also [`opus_packet_get_samples_per_frame`].
         */
        pub fn get_samples_per_frame(&self) -> Result<u32, InvalidPacket> {
            // SAFETY: The pointer points to a valid slice of data, the length is derived from the
            // slice and the slice is not empty
            let samples = unsafe {
                let len = self.data.len().saturating_as();
                let data = self.data.as_ptr();
                opus_packet_get_samples_per_frame(data, len)
            };
            if samples < 0 {
                debug_assert_eq!(samples, OPUS_INVALID_PACKET);
                Err(InvalidPacket {})
            } else {
                Ok(samples.saturating_as())
            }
        }
    }
}

#[cfg(feature = "decode")]
pub use decode::*;

#[cfg(feature = "encode")]
mod encode {
    use super::*;

    /// Error from encoding opus data.
    #[derive(Debug, PartialEq)]
    pub struct EncoderError {
        error_code: c_int,
    }

    unsafe impl RawOpusError for EncoderError {
        fn numeric(&self) -> c_int {
            self.error_code
        }
    }

    impl core::fmt::Display for EncoderError {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(self.message())
        }
    }

    impl core::error::Error for EncoderError {
        fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
            None
        }
    }

    /// Application type hint for the encoder.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
    #[repr(i32)]
    pub enum Application {
        /// Optimise for high-fidelity audio.
        Audio = OPUS_APPLICATION_AUDIO as i32,
        /// Optimise for voice.
        Voice = OPUS_APPLICATION_VOIP as i32,
        /// Optimise for low-delay applications.
        LowDelay = OPUS_APPLICATION_RESTRICTED_LOWDELAY as i32,
    }

    /// Signal type hint for the encoder.
    #[derive(Copy, Clone, Debug, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
    #[repr(i32)]
    pub enum Signal {
        /// Automatic signal type detection.
        Auto = OPUS_AUTO as i32,
        /// Bias toward voice-optimised modes.
        Voice = OPUS_SIGNAL_VOICE as i32,
        /// Bias toward music-optimised modes.
        Music = OPUS_SIGNAL_MUSIC as i32,
    }

    /// Opus encoder.
    #[derive(Debug)]
    pub struct Encoder {
        encoder: OpusEncoder,
        channels: Channels,
        frame_size: i32,
    }

    impl Encoder {
        /// Valid Opus frame sizes in samples per channel.
        pub const VALID_FRAME_SIZES: &'static [i32] = &[120, 240, 480, 960, 1920, 2880];

        /**
         * Construct encoder from requested sampling rate, number of channels, application and
         * frame size.
         *
         * The frame size is the number of samples per channel per encode call. Valid values are
         * 120, 240, 480, 960, 1920, 2880 (corresponding to 2.5 – 60 ms at 48 kHz).
         *
         * Returns an error if stereo is requested without the `stereo` feature.
         *
         * See also [`opus_encoder_get_size`] and [`opus_encoder_init`].
         */
        pub fn new(
            freq: SamplingRate,
            channels: Channels,
            application: Application,
            frame_size: i32,
        ) -> Result<Self, EncoderError> {
            if !cfg!(feature = "stereo") && channels == Channels::Stereo {
                return Err(EncoderError {
                    error_code: OPUS_ALLOC_FAIL,
                });
            }
            let mut encoder = Encoder {
                encoder: OpusEncoder::default(),
                channels,
                frame_size,
            };
            let ch = channels.channels().into();
            // SAFETY: The number of channels can be only one or two as required
            let size = unsafe { opus_encoder_get_size(ch) };
            assert!(
                core::mem::size_of::<OpusEncoder>() >= size.try_into().unwrap(),
                "OpusEncoder struct is too small!"
            );
            // SAFETY: encoder.encoder points to a correct sized chunk of memory
            let error_code =
                unsafe { opus_encoder_init(&mut encoder.encoder, freq.into(), ch, application.into()) };
            if error_code != OPUS_OK.try_into().unwrap() {
                Err(EncoderError { error_code })
            } else {
                Ok(encoder)
            }
        }

        /**
         * Encode PCM samples into an Opus packet.
         *
         * The input slice must contain exactly `frame_size * channels` samples (interleaved for
         * stereo). Returns the encoded packet as a byte slice.
         *
         * See also [`opus_encode`].
         */
        pub fn encode<'out>(
            &mut self,
            input: &[i16],
            output: &'out mut [u8],
        ) -> Result<&'out [u8], EncoderError> {
            let ch: i32 = self.channels.channels().into();
            let expected_input = self.frame_size * ch;
            if input.len() < expected_input as usize {
                return Err(EncoderError {
                    error_code: OPUS_BAD_ARG,
                });
            }
            let max_data_bytes: i32 = output.len().saturating_as();
            // SAFETY: All pointers point to valid slices of the correct lengths
            let result = unsafe {
                let input_ptr = input.as_ptr();
                let output_ptr = output.as_mut_ptr();
                opus_encode(
                    &mut self.encoder,
                    input_ptr,
                    self.frame_size,
                    output_ptr,
                    max_data_bytes,
                )
            };
            if result < 0 {
                Err(EncoderError {
                    error_code: result,
                })
            } else {
                Ok(&output[..result as usize])
            }
        }

        /// Return the frame size in samples per channel.
        pub fn frame_size(&self) -> i32 {
            self.frame_size
        }

        /// Return the number of channels.
        pub fn channels(&self) -> Channels {
            self.channels
        }
    }

    // Encoder control wrappers using opus_encoder_ctl
    macro_rules! encoder_ctl {
        ($self:ident, $request:expr) => {{
            // SAFETY: The encoder pointer is valid and the request matches the argument types
            let ret = unsafe { opus_encoder_ctl(&mut $self.encoder, $request as c_int) };
            if ret != OPUS_OK.try_into().unwrap() {
                return Err(EncoderError { error_code: ret });
            }
            Ok(())
        }};
        ($self:ident, $request:expr, $arg:expr) => {{
            // SAFETY: The encoder pointer is valid and the request matches the argument types
            let ret = unsafe { opus_encoder_ctl(&mut $self.encoder, $request as c_int, $arg) };
            if ret != OPUS_OK.try_into().unwrap() {
                return Err(EncoderError { error_code: ret });
            }
            Ok(())
        }};
    }

    #[allow(non_snake_case)]
    impl Encoder {
        /// Set the bitrate in bits per second. Use `OPUS_AUTO` for default.
        /// See also `OPUS_SET_BITRATE`.
        pub fn set_bitrate(&mut self, bitrate: i32) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_BITRATE_REQUEST, bitrate)
        }

        /// Set the computational complexity (0-10).
        /// See also `OPUS_SET_COMPLEXITY_REQUEST`.
        pub fn set_complexity(&mut self, complexity: i32) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_COMPLEXITY_REQUEST, complexity)
        }

        /// Set the signal type hint.
        /// See also `OPUS_SET_SIGNAL_REQUEST`.
        pub fn set_signal(&mut self, signal: Signal) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_SIGNAL_REQUEST, signal as i32)
        }

        /// Enable or disable in-band forward error correction.
        /// See also `OPUS_SET_INBAND_FEC_REQUEST`.
        pub fn set_inband_fec(&mut self, enabled: bool) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_INBAND_FEC_REQUEST, enabled as i32)
        }

        /// Enable or disable discontinuous transmission.
        /// See also `OPUS_SET_DTX_REQUEST`.
        pub fn set_dtx(&mut self, enabled: bool) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_DTX_REQUEST, enabled as i32)
        }

        /// Set the expected packet loss percentage.
        /// See also `OPUS_SET_PACKET_LOSS_PERC_REQUEST`.
        pub fn set_packet_loss_perc(&mut self, perc: i32) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_PACKET_LOSS_PERC_REQUEST, perc)
        }

        /// Enable or disable variable bitrate.
        /// See also `OPUS_SET_VBR_REQUEST`.
        pub fn set_vbr(&mut self, enabled: bool) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_VBR_REQUEST, enabled as i32)
        }

        /// Enable or disable constrained VBR.
        /// See also `OPUS_SET_VBR_CONSTRAINT_REQUEST`.
        pub fn set_vbr_constraint(&mut self, enabled: bool) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_VBR_CONSTRAINT_REQUEST, enabled as i32)
        }

        /// Set the maximum bandwidth.
        /// See also `OPUS_SET_MAX_BANDWIDTH_REQUEST`.
        pub fn set_max_bandwidth(&mut self, bandwidth: i32) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_MAX_BANDWIDTH_REQUEST, bandwidth)
        }

        /// Force the encoder to use a specific number of channels.
        /// Use `OPUS_AUTO` for automatic detection.
        /// See also `OPUS_SET_FORCE_CHANNELS_REQUEST`.
        pub fn set_force_channels(&mut self, channels: i32) -> Result<(), EncoderError> {
            encoder_ctl!(self, OPUS_SET_FORCE_CHANNELS_REQUEST, channels)
        }
    }
}

#[cfg(feature = "encode")]
pub use encode::*;

/**
 * Number of channels for opus decoder.
 *
 * Note that stereo decoders cannot be created if stereo feature has not been enabled.
 */
#[derive(Copy, Clone, Debug, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
pub enum Channels {
    /// Select mono audio.
    Mono = 1,
    /// Select stereo audio. Samples are interleaved.
    Stereo = 2,
}

impl Channels {
    /// Return the number of channels.
    pub fn channels(&self) -> u8 {
        (*self).into()
    }
}

/**
 * Sampling rate.
 *
 * Only valid sampling rates can be presented.
 */
#[derive(Copy, Clone, Debug, Eq, PartialEq, IntoPrimitive, TryFromPrimitive)]
#[repr(i32)]
pub enum SamplingRate {
    /// 8 kHz sampling rate.
    F8k = 8000,
    /// 12 kHz sampling rate.
    F12k = 12000,
    /// 16 kHz sampling rate.
    F16k = 16000,
    /// 24 kHz sampling rate.
    F24k = 24000,
    /// 48 kHz sampling rate.
    F48k = 48000,
}

impl SamplingRate {
    /// Creates sampling rate that is the same or higher than the requested value up to 48 kHz.
    pub fn closest(value: i32) -> Self {
        use SamplingRate::*;
        if value <= F8k.into() {
            F8k
        } else if value <= F12k.into() {
            F12k
        } else if value <= F16k.into() {
            F16k
        } else if value <= F24k.into() {
            F24k
        } else {
            F48k
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::string::ToString;
    use core::error::Error;

    #[cfg(feature = "decode")]
    mod decode_tests {
        use super::*;

        #[test]
        fn create_decoder() {
            let decoder = Decoder::new(SamplingRate::F8k, Channels::Mono);
            assert!(decoder.is_ok());
        }

        #[test]
        fn create_decoder_stereo() {
            let decoder = Decoder::new(SamplingRate::F16k, Channels::Stereo);
            if cfg!(feature = "stereo") {
                assert!(decoder.is_ok());
            } else {
                assert!(decoder.is_err());
                assert_eq!(decoder.unwrap_err().numeric(), OPUS_ALLOC_FAIL);
            }
        }

        #[test]
        fn test_decoder_with_zero_length_packet() {
            const DATA: [u8; 0] = [0u8; 0];
            let mut decoder = Decoder::new(SamplingRate::F8k, Channels::Mono).unwrap();
            let result = decoder.get_nb_samples(&DATA);
            assert_eq!(
                result,
                Err(DecoderError {
                    error_code: OPUS_BAD_ARG
                })
            );
            let error = result.unwrap_err();
            assert_eq!(error.numeric(), OPUS_BAD_ARG);
            assert!(error.source().is_none());
            assert_eq!(error.to_string(), "invalid argument");
            // Passing empty slice (-> null) is a valid input for decoding
            let mut output = [0i16; 100];
            let result = decoder.decode(&DATA, &mut output);
            assert_eq!(result.unwrap().len(), output.len());
            for v in output {
                assert_eq!(v, 0);
            }
            // However empty slice for output is not
            let mut output = [0i16; 0];
            let result = decoder.decode(&[0, 0, 0, 0, 0], &mut output);
            assert_eq!(
                result,
                Err(DecoderError {
                    error_code: OPUS_BAD_ARG
                })
            );
            let error = result.unwrap_err();
            assert_eq!(error.numeric(), OPUS_BAD_ARG);
            assert!(error.source().is_none());
            assert_eq!(error.to_string(), "invalid argument");
        }

        #[test]
        fn test_decoder_with_zero_packet() {
            const DATA: [u8; 8] = [0x00u8; 8];
            let mut decoder = Decoder::new(SamplingRate::F8k, Channels::Mono).unwrap();
            assert_eq!(decoder.get_nb_samples(&DATA), Ok(80));
            let mut output = [0i16; 80];
            assert_eq!(decoder.decode(&DATA, &mut output).unwrap().len(), 80);
        }

        #[test]
        fn test_decoder_with_0xff_packet() {
            const DATA: [u8; 8] = [0xffu8; 8];
            let mut decoder = Decoder::new(SamplingRate::F8k, Channels::Mono).unwrap();
            let result = decoder.get_nb_samples(&DATA);
            assert_eq!(
                result,
                Err(DecoderError {
                    error_code: OPUS_INVALID_PACKET
                })
            );
            let error = result.unwrap_err();
            assert_eq!(error.numeric(), OPUS_INVALID_PACKET);
            assert!(error.source().is_none());
            assert_eq!(error.to_string(), "corrupted stream");
            let mut output = [0i16; 80];
            let result = decoder.decode(&DATA, &mut output);
            assert_eq!(
                result,
                Err(DecoderError {
                    error_code: OPUS_INVALID_PACKET
                })
            );
            let error = result.unwrap_err();
            assert_eq!(error.numeric(), OPUS_INVALID_PACKET);
            assert!(error.source().is_none());
            assert_eq!(error.to_string(), "corrupted stream");
        }

        #[test]
        #[should_panic]
        fn test_zero_length_packet() {
            const DATA: [u8; 0] = [0u8; 0];
            let _packet = OpusPacket::new(&DATA);
        }

        #[test]
        fn test_zero_packet() {
            let data = [0x00u8; 8];
            let packet = OpusPacket::new(&data);
            assert_eq!(packet.get_nb_channels(), Ok(1));
            assert_eq!(packet.get_nb_frames(), Ok(1));
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Narrowband));
            assert_eq!(packet.get_samples_per_frame(), Ok(0));
        }

        #[test]
        fn test_0xff_packet() {
            let data = [0xFFu8; 8];
            let packet = OpusPacket::new(&data);
            assert_eq!(packet.get_nb_channels(), Ok(2));
            assert_eq!(packet.get_nb_frames(), Ok(63));
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Fullband));
            assert_eq!(packet.get_samples_per_frame(), Ok(0));
        }

        #[test]
        fn test_one_length_packet() {
            let packet = OpusPacket::new(&[0xff]);
            assert_eq!(packet.get_nb_channels(), Ok(2));
            let result = packet.get_nb_frames();
            assert_eq!(result, Err(InvalidPacket {}));
            let error = result.unwrap_err();
            assert_eq!(error.numeric(), OPUS_INVALID_PACKET);
            assert!(error.source().is_none());
            assert_eq!(error.to_string(), "corrupted stream");
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Fullband));
            assert_eq!(packet.get_samples_per_frame(), Ok(0));
        }

        #[test]
        fn test_packet_bandwidths() {
            let packet = OpusPacket::new(&[0x00]);
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Narrowband));
            let packet = OpusPacket::new(&[0x20]);
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Mediumband));
            let packet = OpusPacket::new(&[0xB0]);
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Wideband));
            let packet = OpusPacket::new(&[0xC0]);
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Superwideband));
            let packet = OpusPacket::new(&[0xF0]);
            assert_eq!(packet.get_bandwidth(), Ok(Bandwidth::Fullband));
        }
    }

    #[cfg(feature = "encode")]
    mod encode_tests {
        use super::*;

        #[test]
        fn create_encoder_mono() {
            let encoder = Encoder::new(
                SamplingRate::F48k,
                Channels::Mono,
                Application::Audio,
                960,
            );
            assert!(encoder.is_ok());
        }

        #[test]
        fn create_encoder_stereo() {
            let encoder = Encoder::new(
                SamplingRate::F48k,
                Channels::Stereo,
                Application::Audio,
                960,
            );
            if cfg!(feature = "stereo") {
                assert!(encoder.is_ok());
            } else {
                assert!(encoder.is_err());
                assert_eq!(encoder.unwrap_err().numeric(), OPUS_ALLOC_FAIL);
            }
        }

        #[cfg(feature = "decode")]
        #[test]
        fn encode_and_decode_roundtrip() {
            let mut encoder = Encoder::new(
                SamplingRate::F48k,
                Channels::Mono,
                Application::Audio,
                960,
            )
            .unwrap();
            let mut decoder = Decoder::new(SamplingRate::F48k, Channels::Mono).unwrap();

            // Generate a simple sine wave
            let mut pcm = [0i16; 960];
            for (i, sample) in pcm.iter_mut().enumerate() {
                *sample = (i16::MAX as f64 * (2.0 * core::f64::consts::PI * 440.0 * i as f64 / 48000.0).sin()) as i16;
            }

            let mut packet_buf = [0u8; 2000];
            let packet = encoder.encode(&pcm, &mut packet_buf).unwrap();
            assert!(!packet.is_empty());
            assert!(packet.len() < 2000);

            let mut output = [0i16; 960];
            let decoded = decoder.decode(packet, &mut output).unwrap();
            assert_eq!(decoded.len(), 960);
        }

        #[test]
        fn test_encoder_controls() {
            let mut encoder = Encoder::new(
                SamplingRate::F48k,
                Channels::Mono,
                Application::Audio,
                960,
            )
            .unwrap();
            assert!(encoder.set_bitrate(64000).is_ok());
            assert!(encoder.set_complexity(5).is_ok());
            assert!(encoder.set_signal(Signal::Auto).is_ok());
            assert!(encoder.set_inband_fec(false).is_ok());
            assert!(encoder.set_dtx(false).is_ok());
            assert!(encoder.set_packet_loss_perc(0).is_ok());
            assert!(encoder.set_vbr(true).is_ok());
            assert!(encoder.set_vbr_constraint(false).is_ok());
            assert!(encoder.set_force_channels(OPUS_AUTO).is_ok());
        }

        #[test]
        fn encoder_invalid_frame_size_fails_at_encode() {
            let mut encoder = Encoder::new(
                SamplingRate::F48k,
                Channels::Mono,
                Application::Audio,
                960,
            )
            .unwrap();
            // Pass a 0-size input to trigger BAD_ARG
            let mut output = [0u8; 200];
            let result = encoder.encode(&[], &mut output);
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().numeric(), OPUS_BAD_ARG);
        }
    }

    #[test]
    fn sampling_rate() {
        assert_eq!(SamplingRate::closest(8_000), SamplingRate::F8k);
        assert_eq!(SamplingRate::closest(12_000), SamplingRate::F12k);
        assert_eq!(SamplingRate::closest(16_000), SamplingRate::F16k);
        assert_eq!(SamplingRate::closest(24_000), SamplingRate::F24k);
        assert_eq!(SamplingRate::closest(48_000), SamplingRate::F48k);
    }

    #[test]
    fn sampling_rate_from_primitive() {
        assert!(SamplingRate::try_from(1_000).is_err());
        assert!(SamplingRate::try_from(8_000).is_ok());
        assert!(SamplingRate::try_from(12_000).is_ok());
        assert!(SamplingRate::try_from(16_000).is_ok());
        assert!(SamplingRate::try_from(24_000).is_ok());
        assert!(SamplingRate::try_from(48_000).is_ok());
        assert!(SamplingRate::try_from(64_000).is_err());
    }

    #[test]
    fn channels_from_primitive() {
        assert!(Channels::try_from(0).is_err());
        assert!(Channels::try_from(1).is_ok());
        assert!(Channels::try_from(2).is_ok());
        for channels in 3..=255 {
            assert!(Channels::try_from(channels).is_err());
        }
    }

    #[cfg(all(feature = "encode", feature = "decode"))]
    mod external_tests {
        extern crate std;
        use super::*;
        use alloc::vec::Vec;

        fn generate_sine(frequency: f64, sample_rate: i32, amplitude: f64, num_samples: usize) -> Vec<i16> {
            let nyquist = sample_rate as f64 / 2.0;
            let freq = if frequency >= nyquist { nyquist * 0.99 } else { frequency };
            let two_pi = core::f64::consts::PI * 2.0;
            (0..num_samples)
                .map(|i| {
                    let t = i as f64 / sample_rate as f64;
                    (amplitude * (two_pi * freq * t).sin()) as i16
                })
                .collect()
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

        fn write_ogg_page(
            writer: &mut oggopus_embedded::prelude::OggWriter,
            packet: &[u8],
            samples: u16,
            is_last: bool,
            file: &mut std::fs::File,
            page_buf: &mut [u8],
        ) {
            use std::io::Write;
            let written = writer.write_packet(packet, samples, is_last, page_buf).unwrap();
            file.write_all(&page_buf[..written]).unwrap();
        }

        fn decode_with_opusdec(opus_path: &std::path::Path) -> Vec<i16> {
            let tmp = std::env::temp_dir();
            let stem = opus_path.file_stem().unwrap().to_string_lossy();
            let raw_path = tmp.join(std::ffi::OsStr::new(&std::format!("{}.raw", stem)));

            let output = std::process::Command::new("opusdec")
                .arg("--quiet")
                .arg(opus_path)
                .arg(&raw_path)
                .output()
                .expect("opusdec not found -- install opus-tools");

            assert!(
                output.status.success(),
                "opusdec failed:\nstderr: {}",
                alloc::string::String::from_utf8_lossy(&output.stderr),
            );

            let raw_data = std::fs::read(&raw_path).unwrap();
            let samples: Vec<i16> = raw_data
                .chunks_exact(2)
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                .collect();

            let _ = std::fs::remove_file(&raw_path);
            samples
        }

        #[test]
        fn external_mono_64kbps() {
            let sample_rate = 48000i32;
            let frame_size = 960i32;
            let total_frames = 25;
            let total_samples = (frame_size * total_frames) as usize;
            let pre_skip: u16 = 312; // libopus lookahead at 48 kHz

            // Generate a 440 Hz sine wave at half amplitude
            let pcm = generate_sine(440.0, sample_rate, 0.5 * i16::MAX as f64, total_samples);

            let mut encoder = Encoder::new(
                SamplingRate::F48k,
                Channels::Mono,
                Application::Audio,
                frame_size,
            )
            .unwrap();
            encoder.set_bitrate(64000).unwrap();

            let mut writer = oggopus_embedded::prelude::OggWriter::new(42, pre_skip);

            let tmp = std::env::temp_dir();
            let opus_path = tmp.join("external_mono_64kbps.opus");

            {
                let mut opus_file = std::fs::File::create(&opus_path).unwrap();
                let mut page_buf = [0u8; 8192];

                let header = oggopus_embedded::opus::OpusHeader {
                    version: 1,
                    channels: oggopus_embedded::opus::ChannelMapping::Family0 { channels: 1 },
                    pre_skip,
                    sample_rate: sample_rate as u32,
                    output_gain: 0,
                };
                let written = writer.write_header(&header, &mut page_buf).unwrap();
                use std::io::Write;
                opus_file.write_all(&page_buf[..written]).unwrap();

                let written = writer
                    .write_tags("rust-opus-embedded", &[], &mut page_buf)
                    .unwrap();
                opus_file.write_all(&page_buf[..written]).unwrap();

                let mut enc_buf = [0u8; 2000];
                for frame in 0..total_frames {
                    let start = (frame * frame_size) as usize;
                    let packet = encoder
                        .encode(&pcm[start..start + frame_size as usize], &mut enc_buf)
                        .unwrap();
                    let is_last = frame == total_frames - 1;
                    write_ogg_page(&mut writer, packet, frame_size as u16, is_last, &mut opus_file, &mut page_buf);
                }
            }

            let decoded_samples = decode_with_opusdec(&opus_path);

            let pcm_after_skip = &pcm[pre_skip as usize..];
            let decoded_after_skip = &decoded_samples[pre_skip as usize..];
            let snr = compute_snr(pcm_after_skip, decoded_after_skip);
            let min_len = pcm_after_skip.len().min(decoded_after_skip.len());
            let max_error = pcm_after_skip[..min_len]
                .iter()
                .zip(decoded_after_skip[..min_len].iter())
                .map(|(a, b)| (a - b).unsigned_abs() as u32)
                .max()
                .unwrap_or(0);
            let sample_count_diff =
                (decoded_after_skip.len() as isize - pcm_after_skip.len() as isize).unsigned_abs();

            std::println!(
                "external_mono_64kbps: SNR={:.1} dB, max_error={}, decoded_after_skip={}, original_after_skip={}, diff={}",
                snr, max_error, decoded_after_skip.len(), pcm_after_skip.len(), sample_count_diff,
            );

            assert!(snr > 10.0, "SNR too low: {:.1} dB (expect >10 dB)", snr);
            assert!(
                sample_count_diff <= frame_size as usize,
                "Sample count differs by more than one frame: {} vs {} (diff={})",
                decoded_samples.len(),
                pcm_after_skip.len(),
                sample_count_diff,
            );

            let _ = std::fs::remove_file(&opus_path);
        }

        #[test]
        fn external_mono_silence_no_errors() {
            let sample_rate = 48000i32;
            let frame_size = 960i32;
            let total_frames = 10;
            let total_samples = (frame_size * total_frames) as usize;
            let pre_skip: u16 = 312; // libopus lookahead at 48 kHz

            let pcm = alloc::vec![0i16; total_samples];

            let mut encoder = Encoder::new(
                SamplingRate::F48k,
                Channels::Mono,
                Application::Audio,
                frame_size,
            )
            .unwrap();
            encoder.set_bitrate(32000).unwrap();

            let mut writer = oggopus_embedded::prelude::OggWriter::new(1, pre_skip);

            let tmp = std::env::temp_dir();
            let opus_path = tmp.join("external_silence.opus");

            {
                let mut opus_file = std::fs::File::create(&opus_path).unwrap();
                let mut page_buf = [0u8; 4096];

                let header = oggopus_embedded::opus::OpusHeader {
                    version: 1,
                    channels: oggopus_embedded::opus::ChannelMapping::Family0 { channels: 1 },
                    pre_skip,
                    sample_rate: sample_rate as u32,
                    output_gain: 0,
                };
                let written = writer.write_header(&header, &mut page_buf).unwrap();
                use std::io::Write;
                opus_file.write_all(&page_buf[..written]).unwrap();

                let written = writer.write_tags("test", &[], &mut page_buf).unwrap();
                opus_file.write_all(&page_buf[..written]).unwrap();

                let mut enc_buf = [0u8; 2000];
                for frame in 0..total_frames {
                    let start = (frame * frame_size) as usize;
                    let packet = encoder
                        .encode(&pcm[start..start + frame_size as usize], &mut enc_buf)
                        .unwrap();
                    let is_last = frame == total_frames - 1;
                    write_ogg_page(&mut writer, packet, frame_size as u16, is_last, &mut opus_file, &mut page_buf);
                }
            }

            let decoded_samples = decode_with_opusdec(&opus_path);

            // opusdec raw output includes pre-skip samples; skip them for comparison.
            let decoded_after_skip = &decoded_samples[pre_skip as usize..];
            let max_abs = decoded_after_skip.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            assert!(
                max_abs < 100,
                "Silence decoded with high amplitude: max_abs={}",
                max_abs
            );

            let _ = std::fs::remove_file(&opus_path);
        }

        #[test]
        fn external_tones_multiple_bitrates() {
            let sample_rate = 48000i32;
            let frame_size = 960i32;
            let total_frames = 20;
            let total_samples = (frame_size * total_frames) as usize;
            let pre_skip: u16 = 312; // libopus lookahead at 48 kHz

            let pcm = generate_sine(1000.0, sample_rate, 0.3 * i16::MAX as f64, total_samples);

            for bitrate in &[24000, 48000, 96000] {
                let mut encoder = Encoder::new(
                    SamplingRate::F48k,
                    Channels::Mono,
                    Application::Audio,
                    frame_size,
                )
                .unwrap();
                encoder.set_bitrate(*bitrate).unwrap();

                let mut writer = oggopus_embedded::prelude::OggWriter::new(1, pre_skip);

                let tmp = std::env::temp_dir();
                let opus_path = tmp.join(std::format!("external_{}bps.opus", bitrate));

                {
                    let mut opus_file = std::fs::File::create(&opus_path).unwrap();
                    let mut page_buf = [0u8; 8192];

                    let header = oggopus_embedded::opus::OpusHeader {
                        version: 1,
                        channels: oggopus_embedded::opus::ChannelMapping::Family0 { channels: 1 },
                        pre_skip,
                        sample_rate: sample_rate as u32,
                        output_gain: 0,
                    };
                    let written = writer.write_header(&header, &mut page_buf).unwrap();
                    use std::io::Write;
                    opus_file.write_all(&page_buf[..written]).unwrap();

                    let written = writer.write_tags("test", &[], &mut page_buf).unwrap();
                    opus_file.write_all(&page_buf[..written]).unwrap();

                    let mut enc_buf = [0u8; 2000];
                    for frame in 0..total_frames {
                        let start = (frame * frame_size) as usize;
                        let packet = encoder
                            .encode(&pcm[start..start + frame_size as usize], &mut enc_buf)
                            .unwrap();
                        let is_last = frame == total_frames - 1;
                        write_ogg_page(
                            &mut writer,
                            packet,
                            frame_size as u16,
                            is_last,
                            &mut opus_file,
                            &mut page_buf,
                        );
                    }
                }

                let decoded_samples = decode_with_opusdec(&opus_path);

                let pcm_after_skip = &pcm[pre_skip as usize..];
                let decoded_after_skip = &decoded_samples[pre_skip as usize..];
                let snr = compute_snr(pcm_after_skip, decoded_after_skip);

                std::println!(
                    "external_{}bps: SNR={:.1} dB, decoded_after_skip={}, pcm_after_skip={}",
                    bitrate, snr, decoded_after_skip.len(), pcm_after_skip.len()
                );

                assert!(
                    snr > 6.0,
                    "SNR too low at {} bps: {:.1} dB",
                    bitrate, snr
                );

                let _ = std::fs::remove_file(&opus_path);
            }
        }
    }
}
