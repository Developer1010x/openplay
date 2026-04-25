# openplay-pipeline

GStreamer pipeline construction for all screen casting paths. Wraps GStreamer element creation, linking, and lifecycle management behind typed Rust structs.

## Pipelines

**`SenderPipeline`** (WebRTC sender)
`[capture] → capsfilter(fps) → queue → encoder → h264 capsfilter → rtph264pay → queue → webrtcbin`

**`ReceiverPipeline`** (WebRTC receiver)
`webrtcbin → [pad-added] → rtph264depay → h264parse → decoder → videoconvert → rgba capsfilter → appsink`

**`AirPlaySenderPipeline`**
`[capture] → capsfilter(fps) → queue → encoder → h264 capsfilter → h264parse → appsink`
The appsink emits NAL units; the casting code reads them and sends them over the AirPlay mirror stream.

**`MiracastSenderPipeline`**
`[capture] → capsfilter(fps) → queue → encoder → h264parse → mpegtsmux(align=7) → rtpmp2tpay → udpsink`
Streams RTP/MPEG-TS over UDP to the Miracast receiver's IP and port.

## Encoder selection

`probe_best_encoder()` queries the GStreamer registry at runtime. Priority per platform:

- Linux: VA-API (`vah264enc`) → NVENC (`nvh264enc`) → x264
- macOS: VideoToolbox (`vtenc_h264`) → x264
- Windows: Media Foundation (`mfh264enc`) → NVENC → x264

All encoder configurations are applied through `configure_encoder()`, which sets low-latency properties appropriate for each encoder type.

## CaptureConfig

Carries the platform-specific capture parameters into pipeline constructors.

- Linux: `pw_fd` (PipeWire file descriptor), `node_id`, width, height, framerate
- Windows/macOS: width, height, framerate only (GStreamer elements handle capture internally)

## Initialization

`openplay_pipeline::init()` must be called once before constructing any pipeline. It calls `gstreamer::init()`.

## Tests

```bash
cargo test -p openplay-pipeline
```
