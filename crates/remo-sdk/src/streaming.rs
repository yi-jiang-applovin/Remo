//! Streaming infrastructure: StreamSender for pushing frames, MirrorSession lifecycle.
//!
//! Not currently wired into `server.rs` — the legacy length-prefixed
//! connection handler that called `run_mirror_loop` was removed in the CDP
//! rewrite's Phase 3 cutover. Per the rewrite plan's "Screencast, dual path"
//! section, this high-fidelity H.264 path is meant to survive as a
//! `Remo.startMirror`/`Remo.stopMirror` CDP extension (Track A) — pushing
//! `StreamFrame`s as CDP events through a connection's `EventSink` instead of
//! through `remo_transport::WriteHalf` — rather than being dropped outright
//! (dropping it would be a real fidelity regression versus
//! `Page.startScreencast`'s baseline JPEG screencast). That porting work is
//! tracked separately and hasn't landed yet; this module is kept, unchanged
//! and reachable via `remo_sdk::{MirrorSession, StreamSender, run_mirror_loop}`,
//! as its basis.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use remo_protocol::{stream_flags, Message, StreamFrame};
use remo_transport::WriteHalf;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Handle for pushing StreamFrames to a client connection.
///
/// Wraps `Arc<Mutex<WriteHalf>>` so both the response handler and the
/// streaming pipeline can write to the same connection.
#[derive(Clone)]
pub struct StreamSender {
    write_half: Arc<Mutex<WriteHalf>>,
}

impl StreamSender {
    pub fn new(write_half: Arc<Mutex<WriteHalf>>) -> Self {
        Self { write_half }
    }

    /// Send a StreamFrame to the client.
    pub async fn send_frame(
        &self,
        frame: StreamFrame,
    ) -> Result<(), remo_transport::TransportError> {
        let mut w = self.write_half.lock().await;
        w.send(Message::StreamFrame(frame)).await
    }

    /// Send a non-stream message (for responses during streaming).
    pub async fn send_message(&self, msg: Message) -> Result<(), remo_transport::TransportError> {
        let mut w = self.write_half.lock().await;
        w.send(msg).await
    }
}

/// Active mirror streaming session.
pub struct MirrorSession {
    pub stream_id: u32,
    running: Arc<AtomicBool>,
    sequence: AtomicU32,
}

impl MirrorSession {
    pub fn new(stream_id: u32) -> Self {
        Self {
            stream_id,
            running: Arc::new(AtomicBool::new(true)),
            sequence: AtomicU32::new(0),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    pub fn running_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }

    pub fn next_sequence(&self) -> u32 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
}

/// Start the mirror encoding loop.
///
/// 1. Gets screen info on main thread
/// 2. Creates H.264 encoder
/// 3. Captures frames at target FPS via main-thread GCD dispatch
/// 4. Reads encoded NAL units and sends as StreamFrames
///
/// Returns when the session is stopped or an error occurs.
#[allow(unsafe_code)]
pub async fn run_mirror_loop(session: Arc<MirrorSession>, sender: StreamSender, fps: u32) {
    info!(stream_id = session.stream_id, fps, "starting mirror loop");

    // SAFETY: get_screen_info requires UIKit main-thread execution.
    // On UIKit (iOS) targets, use run_on_main_sync to dispatch to the main thread.
    // On other Apple targets (macOS without UIKit), call directly — the stub returns None
    // without any main-thread requirement, avoiding a GCD dispatch_sync_f deadlock in tests.
    #[cfg(all(target_vendor = "apple", feature = "ios"))]
    let info = {
        tokio::task::spawn_blocking(|| {
            remo_objc::run_on_main_sync(|| unsafe { remo_objc::get_screen_info() })
        })
        .await
        .unwrap_or(None)
    };
    // SAFETY: On non-UIKit targets the stub implementation has no unsafe preconditions.
    #[cfg(not(all(target_vendor = "apple", feature = "ios")))]
    let info = unsafe { remo_objc::get_screen_info() };

    let Some(info) = info else {
        error!("failed to get screen info");
        return;
    };

    let pixel_width = (info.width as f64 * info.scale) as u32;
    let pixel_height = (info.height as f64 * info.scale) as u32;

    let config = remo_objc::EncoderConfig {
        width: pixel_width,
        height: pixel_height,
        fps,
        ..Default::default()
    };

    let (mut encoder, encoded_rx) = match remo_objc::H264Encoder::new(config) {
        Ok(pair) => pair,
        Err(e) => {
            error!("failed to create encoder: {e}");
            return;
        }
    };

    let running = session.running_flag();
    let frame_interval = std::time::Duration::from_micros(1_000_000 / u64::from(fps));

    // Task: read encoded frames from mpsc and send as StreamFrames
    let sender_clone = sender.clone();
    let session_clone = Arc::clone(&session);
    let running_clone = Arc::clone(&running);
    let send_task = tokio::spawn(async move {
        while running_clone.load(Ordering::Relaxed) {
            match encoded_rx.try_recv() {
                Ok(encoded) => {
                    let seq = session_clone.next_sequence();
                    let mut flags = 0u8;
                    if encoded.is_codec_config {
                        flags |= stream_flags::CODEC_CONFIG;
                        if seq == 0 {
                            flags |= stream_flags::STREAM_START;
                        }
                    }
                    if encoded.is_keyframe {
                        flags |= stream_flags::KEYFRAME;
                    }

                    let frame = StreamFrame {
                        stream_id: session_clone.stream_id,
                        sequence: seq,
                        timestamp_us: encoded.timestamp_us,
                        flags,
                        data: encoded.data,
                    };

                    if let Err(e) = sender_clone.send_frame(frame).await {
                        warn!("failed to send stream frame: {e}");
                        session_clone.stop();
                        break;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    session_clone.stop();
                    debug!("encoder channel disconnected");
                    break;
                }
            }
        }
    });

    // Capture loop — render on main thread, encode, sleep
    while running.load(Ordering::Relaxed) {
        let w = info.width;
        let h = info.height;
        let s = info.scale;

        // SAFETY: capture_frame_to_pixel_buffer requires UIKit main-thread execution.
        // On UIKit (iOS) targets, dispatch via run_on_main_sync.
        // On other Apple targets (macOS without UIKit), call directly — stub returns None.
        #[cfg(all(target_vendor = "apple", feature = "ios"))]
        let pixel_buffer = remo_objc::run_on_main_sync(move || unsafe {
            remo_objc::capture_frame_to_pixel_buffer(w, h, s)
        });
        // SAFETY: On non-UIKit targets the stub implementation has no unsafe preconditions.
        #[cfg(not(all(target_vendor = "apple", feature = "ios")))]
        let pixel_buffer = unsafe { remo_objc::capture_frame_to_pixel_buffer(w, h, s) };

        if let Some(pb) = pixel_buffer {
            // SAFETY: pb is a valid CVPixelBufferRef returned by capture_frame_to_pixel_buffer.
            if let Err(e) = unsafe { encoder.encode_frame(pb as *const _) } {
                warn!("encode error: {e}");
            }
            #[link(name = "CoreFoundation", kind = "framework")]
            extern "C" {
                fn CFRelease(cf: *const std::ffi::c_void);
            }
            // SAFETY: pb is a valid CoreFoundation object that we own; releasing after encode.
            unsafe { CFRelease(pb as *const _) };
        }

        tokio::time::sleep(frame_interval).await;
    }

    // Cleanup
    encoder.flush().ok();
    encoder.stop();
    send_task.abort();
    let _ = send_task.await; // Wait for task to release mutex

    let end_frame = StreamFrame {
        stream_id: session.stream_id,
        sequence: session.next_sequence(),
        timestamp_us: 0,
        flags: stream_flags::STREAM_END,
        data: vec![],
    };
    let _ = sender.send_frame(end_frame).await;

    info!(stream_id = session.stream_id, "mirror loop stopped");
}
