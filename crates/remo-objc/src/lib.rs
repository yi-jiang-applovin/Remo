#![allow(unsafe_code)]

pub mod device_info;
pub mod filesystem;
pub mod keychain;
pub mod main_thread;
pub mod screen_capture;
pub mod screenshot;
pub mod user_defaults;
pub mod video_encoder;
pub mod view_tree;

pub use device_info::{get_app_info, get_device_info, AppInfo, DeviceInfo};
pub use filesystem::{delete_path, home_directory, list_directory, read_file, FileEntry};
pub use keychain::{
    delete_keychain_item, get_keychain_item, list_keychain_items, set_keychain_item,
};
pub use main_thread::{is_main_thread, run_on_main_sync};
pub use screen_capture::{capture_frame_to_pixel_buffer, get_screen_info, CaptureInfo};
pub use screenshot::{capture_screenshot, ScreenshotResult};
pub use user_defaults::{
    delete_user_default, get_user_default, list_user_defaults, set_user_default,
};
pub use video_encoder::{avcc_to_annex_b, EncodedFrame, EncoderConfig, H264Encoder};
pub use view_tree::{snapshot_view_tree, Frame, ViewNode};
