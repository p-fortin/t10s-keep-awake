//! Pin a render device's Windows master volume to a percent — a safety cap so the pulse level is
//! reproducible regardless of any manual slider. Re-applied periodically while the engine runs, so
//! a mid-session slider move doesn't stick. Failures are logged, never fatal.
//!
//! `wasapi` finds the device by friendly name and gives us its endpoint ID; the `windows` crate
//! activates `IAudioEndpointVolume` on it (wasapi hides its raw `IMMDevice`, so we re-resolve the
//! device by ID).

use anyhow::{Result, anyhow};
use wasapi::{DeviceEnumerator, Direction};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::core::PCWSTR;

/// Resolve a render endpoint to its raw `IMMDevice` by friendly name.
///
/// Shared with `device_guard`, which needs the same device to reach its property store — hence
/// this being `pub(crate)` rather than folded into `endpoint_volume`.
pub(crate) fn immdevice_by_name(device_name: &str) -> Result<IMMDevice> {
    let _ = wasapi::initialize_mta();
    let e = DeviceEnumerator::new().map_err(|x| anyhow!("enumerator: {x:?}"))?;
    let mut dev_id: Option<String> = None;
    for d in &e
        .get_device_collection(&Direction::Render)
        .map_err(|x| anyhow!("collection: {x:?}"))?
    {
        let d = d.map_err(|x| anyhow!("device: {x:?}"))?;
        if d.get_friendlyname().map_err(|x| anyhow!("name: {x:?}"))? == device_name {
            dev_id = Some(d.get_id().map_err(|x| anyhow!("id: {x:?}"))?);
            break;
        }
    }
    let dev_id = dev_id.ok_or_else(|| anyhow!("render device not found: {device_name:?}"))?;
    unsafe {
        let enumr: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|x| anyhow!("CoCreateInstance: {x:?}"))?;
        let wide: Vec<u16> = dev_id.encode_utf16().chain(std::iter::once(0)).collect();
        enumr
            .GetDevice(PCWSTR(wide.as_ptr()))
            .map_err(|x| anyhow!("GetDevice: {x:?}"))
    }
}

/// Resolve a render endpoint's `IAudioEndpointVolume` by friendly name.
fn endpoint_volume(device_name: &str) -> Result<IAudioEndpointVolume> {
    let immdev = immdevice_by_name(device_name)?;
    unsafe {
        immdev
            .Activate(CLSCTX_ALL, None)
            .map_err(|x| anyhow!("Activate IAudioEndpointVolume: {x:?}"))
    }
}

/// Read the render endpoint's current master volume as a percent (0..=100).
pub fn get_endpoint_volume_percent(device_name: &str) -> Result<u8> {
    let vol = endpoint_volume(device_name)?;
    let scalar = unsafe {
        vol.GetMasterVolumeLevelScalar()
            .map_err(|x| anyhow!("GetMasterVolumeLevelScalar: {x:?}"))?
    };
    Ok((scalar * 100.0).round().clamp(0.0, 100.0) as u8)
}

/// Set the render endpoint `device_name`'s master scalar volume to `percent`/100 (0.0..=1.0).
pub fn set_endpoint_volume_percent(device_name: &str, percent: u8) -> Result<()> {
    let scalar = (percent.min(100) as f32) / 100.0;
    let vol = endpoint_volume(device_name)?;
    unsafe {
        vol.SetMasterVolumeLevelScalar(scalar, std::ptr::null())
            .map_err(|x| anyhow!("SetMasterVolumeLevelScalar: {x:?}"))?;
    }
    Ok(())
}
