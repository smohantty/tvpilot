//! Key injection via Tizen's `libcapi-ui-efl-util`. Mirrors aurum's working
//! path (`efl_util_input_generate_key`). The generator handle is created once
//! at daemon startup and reused for the daemon's lifetime.

use anyhow::{Result, anyhow};
use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::sync::Mutex;
use std::time::Duration;

#[allow(non_camel_case_types)]
type efl_util_inputgen_h = *mut c_void;

const EFL_UTIL_INPUT_DEVTYPE_KEYBOARD: c_uint = 1 << 1;
const EFL_UTIL_ERROR_NONE: c_int = 0;

#[link(name = "capi-ui-efl-util")]
unsafe extern "C" {
    fn efl_util_input_initialize_generator(dev_type: c_uint) -> efl_util_inputgen_h;
    fn efl_util_input_generate_key(
        handle: efl_util_inputgen_h,
        key_name: *const c_char,
        pressed: c_int,
    ) -> c_int;
    fn efl_util_input_deinitialize_generator(handle: efl_util_inputgen_h) -> c_int;
}

/// Raw efl_util handle. `Send + Sync` because all access goes through the
/// generator API, which is safe to call from any thread provided we serialize
/// concurrent presses via the surrounding `Mutex`.
struct Handle(efl_util_inputgen_h);
unsafe impl Send for Handle {}

pub struct KeyInjector {
    handle: Mutex<Handle>,
}

impl KeyInjector {
    pub fn new() -> Result<Self> {
        let h = unsafe { efl_util_input_initialize_generator(EFL_UTIL_INPUT_DEVTYPE_KEYBOARD) };
        if h.is_null() {
            return Err(anyhow!(
                "efl_util_input_initialize_generator returned null — daemon needs the right capabilities"
            ));
        }
        Ok(Self {
            handle: Mutex::new(Handle(h)),
        })
    }

    /// Press then release the given Tizen key name (e.g. "Down", "Return",
    /// "XF86Back"). Repeats `count` times with a small inter-press delay.
    pub fn send_key(&self, key_name: &str, count: u32) -> Result<()> {
        let cname = CString::new(key_name).map_err(|_| anyhow!("key name has nul byte"))?;
        let guard = self.handle.lock().expect("key injector poisoned");
        let h = guard.0;
        for _ in 0..count.max(1) {
            let rc = unsafe { efl_util_input_generate_key(h, cname.as_ptr(), 1) };
            if rc != EFL_UTIL_ERROR_NONE {
                return Err(anyhow!("press '{key_name}' failed: efl_util rc={rc}"));
            }
            std::thread::sleep(Duration::from_millis(20));
            let rc = unsafe { efl_util_input_generate_key(h, cname.as_ptr(), 0) };
            if rc != EFL_UTIL_ERROR_NONE {
                return Err(anyhow!("release '{key_name}' failed: efl_util rc={rc}"));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    }
}

impl Drop for KeyInjector {
    fn drop(&mut self) {
        let mut guard = self.handle.lock().expect("key injector poisoned");
        if !guard.0.is_null() {
            unsafe { efl_util_input_deinitialize_generator(guard.0) };
            guard.0 = std::ptr::null_mut();
        }
    }
}

/// Map user-friendly verb names ("down", "enter", "back", …) to the Tizen
/// efl_util key-name strings actually understood by the input generator.
pub fn resolve_key_name(input: &str) -> Result<&'static str> {
    Ok(match input.to_ascii_lowercase().as_str() {
        "up" => "Up",
        "down" => "Down",
        "left" => "Left",
        "right" => "Right",
        "enter" | "return" | "ok" => "Return",
        "back" | "esc" | "escape" => "XF86Back",
        "home" => "XF86Home",
        "menu" => "XF86Menu",
        "exit" => "XF86Exit",
        "volup" | "volumeup" => "XF86AudioRaiseVolume",
        "voldown" | "volumedown" => "XF86AudioLowerVolume",
        "mute" => "XF86AudioMute",
        "chup" | "channelup" => "XF86RaiseChannel",
        "chdown" | "channeldown" => "XF86LowerChannel",
        "play" => "XF86AudioPlay",
        "pause" => "XF86AudioPause",
        "stop" => "XF86AudioStop",
        "rewind" => "XF86AudioRewind",
        "forward" => "XF86AudioForward",
        "power" => "XF86PowerOff",
        "source" => "XF86Source",
        "info" => "XF86Info",
        "0" => "0",
        "1" => "1",
        "2" => "2",
        "3" => "3",
        "4" => "4",
        "5" => "5",
        "6" => "6",
        "7" => "7",
        "8" => "8",
        "9" => "9",
        "red" => "XF86Red",
        "green" => "XF86Green",
        "yellow" => "XF86Yellow",
        "blue" => "XF86Blue",
        other => return Err(anyhow!("unknown key: '{other}'")),
    })
}
