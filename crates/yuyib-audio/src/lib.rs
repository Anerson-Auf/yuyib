//! Bounded encoded-audio sources and explicit default-device playback.
//!
//! `AudioClip` owns encoded bytes and opens no device. Its decode method creates
//! a one-use `DecodedAudio` stream without output lifecycle effects. `AudioEngine`
//! then owns one explicit default output device and mixes decoded sources.
//!
//! Rodio 0.22.2 is compiled with playback plus WAV PCM, MP3, Ogg Vorbis and
//! FLAC decoding only. MP4/AAC, recording, dithering/noise and other optional
//! codecs are disabled. The encoded-byte budget does not bound decoded duration
//! or decoder CPU work: Rodio streams decode data, so an application accepting
//! untrusted compressed media must impose its own duration and scheduling
//! policy.

#![forbid(unsafe_code)]

use std::{
    error::Error,
    fmt, fs,
    io::{self, Cursor, Read},
    path::Path,
    sync::Arc,
};

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};

type RodioDecoder = Decoder<Cursor<Arc<[u8]>>>;

/// Bounded loading policy for one encoded audio source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AudioLoadLimits {
    /// Maximum accepted encoded byte length.
    max_encoded_bytes: usize,
}

impl AudioLoadLimits {
    /// Creates a non-zero encoded-byte limit.
    ///
    /// # Errors
    ///
    /// Returns `AudioLoadLimitError::ZeroEncodedByteLimit` for zero.
    pub const fn new(max_encoded_bytes: usize) -> Result<Self, AudioLoadLimitError> {
        if max_encoded_bytes == 0 {
            Err(AudioLoadLimitError::ZeroEncodedByteLimit)
        } else {
            Ok(Self { max_encoded_bytes })
        }
    }

    /// Returns the maximum encoded byte count accepted by one load operation.
    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        self.max_encoded_bytes
    }
}

impl Default for AudioLoadLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Invalid `AudioLoadLimits` configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioLoadLimitError {
    /// Encoded input cannot be bounded with zero maximum bytes.
    ZeroEncodedByteLimit,
}

impl fmt::Display for AudioLoadLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("audio encoded-byte limit must be non-zero")
    }
}

impl Error for AudioLoadLimitError {}

/// Failure while reading or bounding an encoded audio source.
#[derive(Debug)]
pub enum AudioLoadError {
    /// Input exceeds the selected encoded-byte limit.
    EncodedByteLimitExceeded {
        /// Maximum accepted bytes.
        maximum: usize,
        /// Observed encoded bytes.
        actual: usize,
    },
    /// The operating system could not inspect the input file.
    FileMetadata(io::Error),
    /// The operating system could not read the input file.
    FileRead(io::Error),
    /// The configured byte limit cannot safely be extended by one probe byte.
    EncodedByteLimitTooLarge {
        /// Requested maximum encoded bytes.
        maximum: usize,
    },
}

impl fmt::Display for AudioLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EncodedByteLimitExceeded { maximum, actual } => write!(
                formatter,
                "encoded audio size {actual} exceeds the configured {maximum}-byte limit"
            ),
            Self::FileMetadata(error) => write!(formatter, "could not inspect audio file: {error}"),
            Self::FileRead(error) => write!(formatter, "could not read audio file: {error}"),
            Self::EncodedByteLimitTooLarge { maximum } => write!(
                formatter,
                "encoded audio limit {maximum} cannot be safely probed for overflow"
            ),
        }
    }
}

impl Error for AudioLoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FileMetadata(error) | Self::FileRead(error) => Some(error),
            Self::EncodedByteLimitExceeded { .. } | Self::EncodedByteLimitTooLarge { .. } => None,
        }
    }
}

/// Immutable bounded encoded audio data, independent from an output device.
#[derive(Clone, Debug)]
pub struct AudioClip {
    bytes: Arc<[u8]>,
}

impl AudioClip {
    /// Creates a clip from caller-owned encoded bytes after enforcing limits.
    ///
    /// This performs no codec decoding and opens no audio output device.
    ///
    /// # Errors
    ///
    /// Returns `AudioLoadError::EncodedByteLimitExceeded` for oversized data.
    pub fn from_bytes(
        bytes: impl Into<Vec<u8>>,
        limits: AudioLoadLimits,
    ) -> Result<Self, AudioLoadError> {
        let bytes = bytes.into();
        if bytes.len() > limits.max_encoded_bytes() {
            return Err(AudioLoadError::EncodedByteLimitExceeded {
                maximum: limits.max_encoded_bytes(),
                actual: bytes.len(),
            });
        }
        Ok(Self {
            bytes: Arc::from(bytes),
        })
    }

    /// Reads one explicitly selected file into bounded encoded data.
    ///
    /// Metadata is checked for early rejection. The file is then read through
    /// a max-plus-one-byte stream cap before allocating its entire content, so
    /// a changing file cannot bypass the selected limit.
    ///
    /// # Errors
    ///
    /// Returns structured metadata, I/O, or encoded-byte-limit failures.
    pub fn load_file(
        path: impl AsRef<Path>,
        limits: AudioLoadLimits,
    ) -> Result<Self, AudioLoadError> {
        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(AudioLoadError::FileMetadata)?;
        let maximum = limits.max_encoded_bytes();
        let maximum_u64 = u64::try_from(maximum)
            .map_err(|_| AudioLoadError::EncodedByteLimitTooLarge { maximum })?;
        if metadata.len() > maximum_u64 {
            return Err(AudioLoadError::EncodedByteLimitExceeded {
                maximum,
                actual: usize::try_from(metadata.len()).unwrap_or(usize::MAX),
            });
        }
        let probe_length = maximum_u64
            .checked_add(1)
            .ok_or(AudioLoadError::EncodedByteLimitTooLarge { maximum })?;
        let mut bytes = Vec::new();
        fs::File::open(path)
            .map_err(AudioLoadError::FileRead)?
            .take(probe_length)
            .read_to_end(&mut bytes)
            .map_err(AudioLoadError::FileRead)?;
        Self::from_bytes(bytes, limits)
    }

    /// Returns retained encoded byte count.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.bytes.len()
    }

    /// Creates one decoder without opening an output device.
    ///
    /// Detection is content based. Enabled codecs are WAV PCM, MP3, Ogg Vorbis
    /// and FLAC.
    ///
    /// # Errors
    ///
    /// Returns `AudioDecodeError` when data is malformed or unsupported. The
    /// encoded-byte limit does not cap decoded duration or decoder CPU work.
    pub fn decode(&self) -> Result<DecodedAudio, AudioDecodeError> {
        Decoder::try_from(Cursor::new(Arc::clone(&self.bytes)))
            .map(DecodedAudio::new)
            .map_err(AudioDecodeError::Backend)
    }
}

/// Failure while decoding an `AudioClip`.
#[derive(Debug)]
pub enum AudioDecodeError {
    /// Rodio could not identify or decode the source with enabled codecs.
    Backend(rodio::decoder::DecoderError),
}

impl fmt::Display for AudioDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => write!(formatter, "audio decode failed: {error}"),
        }
    }
}

impl Error for AudioDecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(error) => Some(error),
        }
    }
}

/// One decoded, one-use source ready to attach to `AudioEngine`.
pub struct DecodedAudio {
    decoder: RodioDecoder,
}

impl DecodedAudio {
    fn new(decoder: RodioDecoder) -> Self {
        Self { decoder }
    }
}

/// Failure while opening a default output device.
#[derive(Debug)]
pub enum AudioOutputError {
    /// Rodio/CPAL could not find or open a usable output stream.
    Backend(rodio::DeviceSinkError),
}

impl fmt::Display for AudioOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(error) => {
                write!(formatter, "audio output initialisation failed: {error}")
            }
        }
    }
}

impl Error for AudioOutputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Backend(error) => Some(error),
        }
    }
}

/// Explicit default output device and mixer lifecycle.
///
/// Dropping this value stops attached playback. No global singleton is used.
pub struct AudioEngine {
    output: MixerDeviceSink,
}

impl AudioEngine {
    /// Opens the platform default output device and mixer.
    ///
    /// # Errors
    ///
    /// Returns `AudioOutputError` if no usable output device or stream exists.
    pub fn open_default() -> Result<Self, AudioOutputError> {
        let mut output =
            DeviceSinkBuilder::open_default_sink().map_err(AudioOutputError::Backend)?;
        output.log_on_drop(false);
        Ok(Self { output })
    }

    /// Attaches one decoded source and returns a controlled playback handle.
    #[must_use]
    pub fn play(&self, source: DecodedAudio) -> AudioPlaybackHandle {
        let player = Player::connect_new(self.output.mixer());
        player.append(source.decoder);
        AudioPlaybackHandle { player }
    }

    /// Decodes a clip and starts one controlled playback instance.
    ///
    /// # Errors
    ///
    /// Returns decode errors without opening or replacing an output device.
    pub fn play_clip(&self, clip: &AudioClip) -> Result<AudioPlaybackHandle, AudioDecodeError> {
        clip.decode().map(|source| self.play(source))
    }

    /// Decodes and starts a fire-and-forget one-shot sound.
    ///
    /// The sound continues through this engine until it ends or the engine is
    /// dropped. Use `play_clip` when later controls are required.
    ///
    /// # Errors
    ///
    /// Returns decode errors without changing output-device lifecycle.
    pub fn play_one_shot(&self, clip: &AudioClip) -> Result<(), AudioDecodeError> {
        let handle = self.play_clip(clip)?;
        handle.player.detach();
        Ok(())
    }
}

/// Invalid controlled-playback input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioControlError {
    /// Volume must be finite and non-negative.
    InvalidVolume(f32),
}

impl fmt::Display for AudioControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVolume(volume) => {
                write!(
                    formatter,
                    "audio volume must be finite and non-negative, got {volume}"
                )
            }
        }
    }
}

impl Error for AudioControlError {}

/// Managed control over one playback instance.
///
/// Dropping this handle stops its attached source.
pub struct AudioPlaybackHandle {
    player: Player,
}

impl AudioPlaybackHandle {
    /// Pauses this playback instance.
    pub fn pause(&self) {
        self.player.pause();
    }

    /// Resumes this playback instance.
    pub fn resume(&self) {
        self.player.play();
    }

    /// Stops this playback instance.
    pub fn stop(&self) {
        self.player.stop();
    }

    /// Returns whether this instance is paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    /// Returns whether no source remains queued.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.player.empty()
    }

    /// Returns current linear volume multiplier.
    #[must_use]
    pub fn volume(&self) -> f32 {
        self.player.volume()
    }

    /// Sets finite non-negative linear volume.
    ///
    /// One is unchanged source level, zero mutes, and larger values can clip.
    ///
    /// # Errors
    ///
    /// Returns `AudioControlError::InvalidVolume` for negative, NaN or infinity.
    pub fn set_volume(&self, volume: f32) -> Result<(), AudioControlError> {
        if !volume.is_finite() || volume < 0.0 {
            return Err(AudioControlError::InvalidVolume(volume));
        }
        self.player.set_volume(volume);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_TEST_FILE: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn load_limits_reject_zero_and_oversized_memory_input() {
        assert_eq!(
            AudioLoadLimits::new(0),
            Err(AudioLoadLimitError::ZeroEncodedByteLimit)
        );
        let limits = AudioLoadLimits::new(3).expect("positive byte limit");
        assert_eq!(
            AudioClip::from_bytes(vec![0_u8; 4], limits)
                .expect_err("oversized bytes must be rejected")
                .to_string(),
            "encoded audio size 4 exceeds the configured 3-byte limit"
        );
    }

    #[test]
    fn clips_retain_bounded_bytes_and_decode_headlessly() {
        let clip = AudioClip::from_bytes(
            vec![1_u8, 2, 3],
            AudioLoadLimits::new(3).expect("positive byte limit"),
        )
        .expect("bounded bytes must load");
        assert_eq!(clip.encoded_len(), 3);
        assert!(matches!(clip.decode(), Err(AudioDecodeError::Backend(_))));
    }

    #[test]
    fn file_loader_enforces_bound_without_audio_device() {
        let suffix = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "yuyib-audio-load-{}-{suffix}.bin",
            std::process::id()
        ));
        std::fs::write(&path, [1_u8, 2, 3]).expect("temporary source bytes must write");

        let clip =
            AudioClip::load_file(&path, AudioLoadLimits::new(3).expect("positive byte limit"))
                .expect("exactly bounded file must load");
        assert_eq!(clip.encoded_len(), 3);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn control_errors_are_headless_and_explicit() {
        assert_eq!(
            AudioControlError::InvalidVolume(-0.5).to_string(),
            "audio volume must be finite and non-negative, got -0.5"
        );
        assert!(matches!(
            AudioControlError::InvalidVolume(f32::NAN),
            AudioControlError::InvalidVolume(value) if value.is_nan()
        ));
    }
}
