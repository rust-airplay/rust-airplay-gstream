/* Does the data this port receives decode when it is fed the way upstream feeds it?
 *
 * Upstream does not hand a decoder a stream: it hands it one buffer per audio payload, with
 * the caps and the `AudioSpecificConfig` set on the source
 * (`renderers/audio_renderer.c:147-164`, `:332-367`). A frame in a buffer is a frame to the
 * decoder; a run of frames in a buffer is not, and that is the difference between a
 * pipeline that plays and one that refuses with "no valid frames found".
 *
 * So this is that pipeline with nothing else in it, reading the dump this port writes — a
 * four-byte little-endian length and then the payload — and pushing each payload as its own
 * buffer. If this decodes, the frames are right and only their delivery was wrong; if it
 * does not, the frames themselves are the problem.
 *
 * Build (MSYS2 ucrt64):
 *   gcc -O1 -o target/eld_probe.exe tools/eld_probe.c \
 *       $(pkg-config --cflags --libs gstreamer-1.0 gstreamer-app-1.0)
 */

#include <gst/app/gstappsink.h>
#include <gst/app/gstappsrc.h>
#include <gst/gst.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

/* Upstream's caps for AAC-ELD, from `audio_renderer.c:68`: the `codec_data` is the
 * `AudioSpecificConfig` — the MPEG-4 configuration a raw frame does not carry — and it is
 * what says ELD, 44100 Hz, stereo, 480 samples a frame. */
static const char *aac_eld_caps =
    "audio/mpeg,mpegversion=(int)4,channels=(int)2,rate=(int)44100,"
    "stream-format=raw,codec_data=(buffer)f8e85000";

static gint buffers_out = 0;
static guint64 bytes_out = 0;
static gint buffers_in = 0;
static struct timespec started;

static double elapsed(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (double) (now.tv_sec - started.tv_sec) + (double) (now.tv_nsec - started.tv_nsec) / 1e9;
}

static GstFlowReturn on_new_sample(GstAppSink *sink, gpointer user_data) {
    GstSample *sample = gst_app_sink_pull_sample(sink);
    if (!sample) {
        return GST_FLOW_ERROR;
    }
    GstBuffer *buffer = gst_sample_get_buffer(sample);
    GstMapInfo map;
    if (gst_buffer_map(buffer, &map, GST_MAP_READ)) {
        buffers_out++;
        bytes_out += map.size;
        if (buffers_out <= 3) {
            printf("  decoded buffer %d: %u bytes\n", buffers_out, (unsigned) map.size);
        }
        gst_buffer_unmap(buffer, &map);
    }
    gst_sample_unref(sample);
    (void) user_data;
    return GST_FLOW_OK;
}

static gboolean on_bus(GstBus *bus, GstMessage *message, gpointer user_data) {
    GError *error = NULL;
    gchar *debug = NULL;
    switch (GST_MESSAGE_TYPE(message)) {
    case GST_MESSAGE_ERROR:
        gst_message_parse_error(message, &error, &debug);
        fprintf(stderr, "ERROR from %s: %s\n", GST_OBJECT_NAME(message->src), error->message);
        if (debug) {
            fprintf(stderr, "   %s\n", debug);
        }
        g_clear_error(&error);
        g_free(debug);
        *(gboolean *) user_data = TRUE;
        break;
    case GST_MESSAGE_EOS:
        printf("EOS after %.2f s\n", elapsed());
        *(gboolean *) user_data = TRUE;
        break;
    default:
        break;
    }
    (void) bus;
    return TRUE;
}

/* One payload of a dump: its bytes and its length, both taken from the file. */
typedef struct {
    guint8 *bytes;
    gsize size;
} frame_t;

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : "target/client-audio.bin";
    gst_init(&argc, &argv);
    clock_gettime(CLOCK_MONOTONIC, &started);

    gchar *contents = NULL;
    gsize length = 0;
    GError *read_error = NULL;
    if (!g_file_get_contents(path, &contents, &length, &read_error)) {
        fprintf(stderr, "cannot read %s: %s\n", path, read_error->message);
        return 1;
    }

    /* The dump's frames, with the start-up markers dropped: they are the four bytes
     * `00 68 34 00` that upstream itself refuses to enqueue (`raop_buffer.c:176-178`). */
    GArray *frames = g_array_new(FALSE, FALSE, sizeof(frame_t));
    gsize at = 0;
    gsize markers = 0;
    while (at + 4 <= length) {
        uint32_t size = (uint32_t) (unsigned char) contents[at] |
                        (uint32_t) (unsigned char) contents[at + 1] << 8 |
                        (uint32_t) (unsigned char) contents[at + 2] << 16 |
                        (uint32_t) (unsigned char) contents[at + 3] << 24;
        at += 4;
        if (at + size > length) {
            break;
        }
        if (size == 4 && memcmp(contents + at, "\x00\x68\x34\x00", 4) == 0) {
            markers++;
            at += size;
            continue;
        }
        frame_t frame;
        frame.size = size;
        frame.bytes = g_malloc(size);
        memcpy(frame.bytes, contents + at, size);
        g_array_append_val(frames, frame);
        at += size;
    }
    printf("%s: %u frames of audio, %u start-up markers\n", path, frames->len, (guint) markers);
    if (frames->len == 0) {
        return 1;
    }
    {
        const frame_t *first = &g_array_index(frames, frame_t, 0);
        printf("first frame: %u bytes, first byte 0x%02x\n", (unsigned) first->size,
               first->bytes[0]);
    }

    /* appsrc, as upstream runs it: a source this program pushes buffers into, with the caps
     * that say what those buffers are. */
    GstElement *pipeline = gst_parse_launch(
        "appsrc name=audio_source ! queue ! avdec_aac ! audioconvert ! audioresample ! "
        "appsink name=out sync=false max-buffers=8 emit-signals=true",
        NULL);
    if (!pipeline) {
        fprintf(stderr, "the pipeline could not be built\n");
        return 1;
    }
    GstElement *appsrc = gst_bin_get_by_name(GST_BIN(pipeline), "audio_source");
    GstElement *appsink = gst_bin_get_by_name(GST_BIN(pipeline), "out");
    GstCaps *caps = gst_caps_from_string(aac_eld_caps);
    g_object_set(appsrc, "caps", caps, "format", GST_FORMAT_TIME, "is-live", FALSE, NULL);
    gst_caps_unref(caps);
    g_signal_connect(appsink, "new-sample", G_CALLBACK(on_new_sample), NULL);

    gboolean stop = FALSE;
    GstBus *bus = gst_element_get_bus(pipeline);
    gst_bus_add_watch(bus, on_bus, &stop);

    if (gst_element_set_state(pipeline, GST_STATE_PLAYING) == GST_STATE_CHANGE_FAILURE) {
        fprintf(stderr, "the pipeline refused to start\n");
        return 1;
    }

    /* One buffer per frame, in order, each with a timestamp of its own. A few hundred is
     * enough to say whether it decodes at all. */
    guint64 timestamp = 0;
    const guint limit = frames->len < 300 ? frames->len : 300;
    for (guint index = 0; index < limit && !stop; index++) {
        const frame_t *frame = &g_array_index(frames, frame_t, index);
        GstBuffer *buffer = gst_buffer_new_allocate(NULL, frame->size, NULL);
        gst_buffer_fill(buffer, 0, frame->bytes, frame->size);
        GST_BUFFER_PTS(buffer) = timestamp;
        timestamp += GST_SECOND * 480 / 44100;
        if (gst_app_src_push_buffer(GST_APP_SRC(appsrc), buffer) != GST_FLOW_OK) {
            fprintf(stderr, "appsrc stopped taking buffers at frame %u\n", index);
            break;
        }
        buffers_in++;
        /* A decoder that is never given the main loop never reports anything. */
        while (g_main_context_iteration(NULL, FALSE)) {
        }
    }
    gst_app_src_end_of_stream(GST_APP_SRC(appsrc));

    gint64 deadline = g_get_monotonic_time() + 20 * G_TIME_SPAN_SECOND;
    while (!stop && g_get_monotonic_time() < deadline) {
        g_main_context_iteration(NULL, TRUE);
    }

    printf("pushed %d buffers, got %d out, %" G_GUINT64_FORMAT " bytes, %.2f s\n", buffers_in,
           buffers_out, bytes_out, elapsed());
    printf("%s\n", buffers_out == 0 ? "NOTHING DECODED" : "DECODED: the frames are right");

    gst_element_set_state(pipeline, GST_STATE_NULL);
    gst_object_unref(bus);
    gst_object_unref(appsrc);
    gst_object_unref(appsink);
    gst_object_unref(pipeline);
    for (guint index = 0; index < frames->len; index++) {
        g_free(g_array_index(frames, frame_t, index).bytes);
    }
    g_array_unref(frames);
    g_free(contents);
    return buffers_out == 0 ? 2 : 0;
}
