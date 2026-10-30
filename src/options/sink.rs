// SPDX-License-Identifier: GPL-3.0-or-later
//! What a sink fragment may name.
//!
//! A sink is a fragment of pipeline description, and pasting a caller's string into a
//! description is how a configuration option becomes a second branch of the pipeline.
//! The rules here are what keeps a fragment an element and its properties.

use gstreamer as gst;
use rust_airplay::RenderError;

/// The tokens of a sink fragment, if it is one this backend may build.
///
/// A fragment names an element and its properties. It may not contain a chain
/// separator or quoting: pasting either into a description is how a caller's string
/// becomes a second branch of the pipeline, and a backend that let a configuration
/// string name an arbitrary element would be one an attacker can point at a file.
///
/// # Errors
///
/// Returns [`RenderError::Unavailable`] when no element of that name exists, and
/// [`RenderError::Failed`] when the fragment is not a sink and its properties.
pub(crate) fn sink_tokens(fragment: &str) -> Result<Vec<String>, RenderError> {
    if fragment.contains('!') {
        return Err(RenderError::Failed(format!(
            "the sink {fragment:?} contains a chain separator"
        )));
    }
    if fragment.contains('"') || fragment.contains('\'') {
        return Err(RenderError::Failed(format!(
            "the sink {fragment:?} quotes a value, which this backend does not parse"
        )));
    }
    let tokens: Vec<String> = fragment.split_whitespace().map(str::to_owned).collect();
    let Some((name, properties)) = tokens.split_first() else {
        return Err(RenderError::Failed("the sink is empty".to_owned()));
    };
    if gst::ElementFactory::find(name).is_none() {
        return Err(RenderError::Unavailable(format!(
            "no GStreamer element named {name} is installed"
        )));
    }
    if let Some(property) = properties.iter().find(|token| !token.contains('=')) {
        return Err(RenderError::Failed(format!(
            "the sink {fragment:?} has {property:?}, which is not a property=value pair"
        )));
    }
    Ok(tokens)
}

/// The tokens of the sink to use, chosen now rather than at the first frame.
///
/// # Errors
///
/// Returns [`RenderError::Unavailable`] when nothing in `candidates` is installed
/// either.
pub(crate) fn resolve_sink(
    fragment: Option<&str>,
    candidates: &[&str],
    kind: &str,
) -> Result<Vec<String>, RenderError> {
    if let Some(fragment) = fragment {
        return sink_tokens(fragment);
    }
    let installed = candidates
        .iter()
        .find(|name| gst::ElementFactory::find(name).is_some());
    match installed {
        Some(name) => sink_tokens(name),
        None => Err(RenderError::Unavailable(format!(
            "no {kind} sink is installed (looked for {})",
            candidates.join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::sink_tokens;
    use rust_airplay::RenderError;

    #[test]
    fn a_sink_fragment_names_one_element_and_its_properties() {
        gstreamer::init().expect("GStreamer");
        let tokens = sink_tokens("filesink location=out.raw").expect("a sink");
        assert_eq!(tokens, ["filesink", "location=out.raw"]);
        // The same element the crate's defaults name.
        assert!(sink_tokens("fakesink").is_ok());
    }

    #[test]
    fn a_sink_cannot_smuggle_a_branch_into_the_pipeline() {
        gstreamer::init().expect("GStreamer");
        // A separator would let a caller add elements the backend never chose.
        let error = sink_tokens("fakesink ! filesink location=x").expect_err("a separator");
        assert!(matches!(error, RenderError::Failed(_)), "{error:?}");
        // Quoting is refused rather than parsed: a value with a space in it has no
        // unambiguous meaning in this form.
        assert!(sink_tokens("filesink location=\"a b\"").is_err());
        // And a property that is not one is a mistake rather than a silent extra
        // element name.
        assert!(sink_tokens("fakesink nope").is_err());
    }

    #[test]
    fn a_sink_that_is_not_installed_is_unavailable_rather_than_fatal() {
        gstreamer::init().expect("GStreamer");
        let error = sink_tokens("no-such-sink-name").expect_err("a missing sink");
        assert!(matches!(error, RenderError::Unavailable(_)), "{error:?}");
    }
}
