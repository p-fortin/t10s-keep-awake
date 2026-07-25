//! "Start with Windows" via the HKCU Run key. Points at the current executable.

use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "T10sKeepAwake";

fn run_key(access: u32) -> Result<RegKey> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey_with_flags(RUN_KEY, access)
        .context("opening HKCU Run key")
}

/// Is the app registered to start at login?
#[allow(dead_code)] // kept as the read side of the Run-key pair
pub fn is_enabled() -> bool {
    run_key(KEY_READ)
        .and_then(|k| k.get_value::<String, _>(VALUE_NAME).map_err(Into::into))
        .is_ok()
}

/// Register (true) or unregister (false) autostart, pointing at this exe.
pub fn set_enabled(enable: bool) -> Result<()> {
    if enable {
        let exe = std::env::current_exe().context("current exe path")?;
        let command = format!("\"{}\"", exe.display());
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(RUN_KEY)
            .context("creating HKCU Run key")?
            .0;
        key.set_value(VALUE_NAME, &command)
            .context("writing autostart value")?;
    } else if let Ok(key) = run_key(KEY_WRITE) {
        // ignore "not found" on delete
        let _ = key.delete_value(VALUE_NAME);
    }
    Ok(())
}
