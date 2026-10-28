#![cfg(target_os = "linux")]
#[path = "support/gpu.rs"]
mod gpu;
#[ignore = "requires a capable Vulkan driver plus validation layer"]
#[test]
fn actual_unorm_damage_and_transparent_clearing_match_reference() {
    gpu::run("unorm").unwrap();
}
#[ignore = "requires a capable Vulkan driver plus validation layer"]
#[test]
fn actual_srgb_compositing_matches_with_documented_conversion_tolerance() {
    gpu::run("srgb").unwrap();
}
#[ignore = "requires a capable Vulkan driver plus validation layer"]
#[test]
fn actual_delayed_read_is_immutable_and_final_static_patch_flushes() {
    gpu::run("held").unwrap();
}
#[ignore = "requires a capable Vulkan driver plus validation layer"]
#[test]
fn actual_replacement_resize_and_old_fences_return_to_plateau() {
    gpu::run("churn").unwrap();
}

#[cfg(target_os = "linux")]
pub(crate) use gpu::{lifetime, loader_abi, resources};
#[cfg(target_os = "linux")]
pub use uw_vk_layer::{DrawOutcome, submit_if_ready, upload_region};
