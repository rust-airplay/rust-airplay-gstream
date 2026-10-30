// SPDX-License-Identifier: GPL-3.0-or-later
//! The pipeline an audio stream is built from.

use gstreamer as gst;
use rust_airplay::{AudioCodec, AudioConfig, RenderError};

/// The caps for a coding, including the configuration its frames do not carry.
///
/// The codec_data is the part that cannot be derived from the frames: it is the
/// AudioSpecificConfig (for AAC) or the magic cookie (for ALAC) that a decoder needs
/// before it can read one. Both values are AirPlay's.
pub(crate) fn audio_caps(config: AudioConfig) -> Result<gst::Caps, RenderError> {
    let text = match config.codec {
        AudioCodec::AacEld => format!(
            "audio/mpeg,mpegversion=(int)4,channels=(int){},rate=(int){},stream-format=raw,\
             codec_data=(buffer)f8e85000",
            config.channels, config.sample_rate
        ),
        AudioCodec::Alac => format!(
            "audio/x-alac,mpegversion=(int)4,channels=(int){},rate=(int){},stream-format=raw,\
             codec_data=(buffer)00000024616c616300000000000001600010280a0e0200ff0000000000000000\
             0000ac44",
            config.channels, config.sample_rate
        ),
        _ => {
            return Err(RenderError::Unavailable(format!(
                "this backend does not decode {:?}",
                config.codec
            )));
        }
    };
    text.parse::<gst::Caps>()
        .map_err(|_| RenderError::Unavailable(format!("GStreamer did not accept the caps {text}")))
}

/// The decoder a coding needs, named rather than searched for.
///
/// decodebin is right for video, which can change coding mid-session, and wrong here: the
/// frame this stream carries has no parser to introduce it, so the decoder has to be
/// chosen from the coding the client named.
fn audio_decoder(codec: AudioCodec) -> Option<&'static str> {
    match codec {
        AudioCodec::AacEld => Some("avdec_aac"),
        AudioCodec::Alac => Some("avdec_alac"),
        _ => None,
    }
}

/// The description one audio pipeline is built from.
///
/// queue between the source and the decoder is what lets the two run on different
/// threads; audioconvert and audioresample are there because a sink rarely takes
/// 44.1 kHz stereo exactly, and a resampler told nothing picks a poor quality.
/// sync=false because a client's clock and a sound card's are not the same clock, and
/// waiting on one to schedule the other is what makes a receiver stutter: the
/// presentation times this backend computes already carry the only timing that matters.
///
/// # Errors
///
/// Returns [`RenderError::Unavailable`] for a coding this backend has no decoder for.
pub(crate) fn audio_description(
    config: AudioConfig,
    sink: &str,
    volume: f64,
) -> Result<String, RenderError> {
    let decoder = audio_decoder(config.codec).ok_or_else(|| {
        RenderError::Unavailable(format!("this backend does not decode {:?}", config.codec))
    })?;
    Ok(format!(
        "appsrc name=audio_source is-live=true format=time ! queue ! {} ! audioconvert ! \
         audioresample quality=10 ! volume name=volume volume={volume} ! {sink} sync=false",
        decoder
    ))
}

#[cfg(test)]
mod tests {
    use super::audio_decoder;
    use rust_airplay::AudioCodec;

    #[test]
    fn every_coding_this_backend_opens_has_a_decoder() {
        assert_eq!(audio_decoder(AudioCodec::AacEld), Some("avdec_aac"));
        assert_eq!(audio_decoder(AudioCodec::Alac), Some("avdec_alac"));
    }
}
