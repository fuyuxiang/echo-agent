//! Native macOS modifier key detection via CoreGraphics.
//!
//! Side-channels around the PTY directly accessing CoreGraphics.

// CoreGraphics CGEventSourceFlagsState — returns the current global
// modifier flags without requiring any special permissions.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceFlagsState(stateID: i32) -> u64;
}

const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: i32 = 1;

// CGEventFlags bitmask constants from <CoreGraphics/CGEventTypes.h>
const K_CG_EVENT_FLAG_MASK_SHIFT: u64 = 0x0002_0000;
const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
const K_CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000; // Option key
const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;

fn flags() -> u64 {
    // SAFETY: CGEventSourceFlagsState is a stable, public CoreGraphics API
    // available since macOS 10.4. Integer in, integer out, no pointers
    // cross the boundary.
    unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE) }
}

/// One CG syscall, all modifier bits decoded.
pub fn snapshot() -> super::ModifierState {
    let f = flags();
    super::ModifierState {
        command: f & K_CG_EVENT_FLAG_MASK_COMMAND != 0,
        option: f & K_CG_EVENT_FLAG_MASK_ALTERNATE != 0,
        shift: f & K_CG_EVENT_FLAG_MASK_SHIFT != 0,
        control: f & K_CG_EVENT_FLAG_MASK_CONTROL != 0,
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn smoke_test_modifier_detection() {
        let s = snapshot();
        let _ = s.command;
        let _ = s.option;
        let _ = s.shift;
        let _ = s.control;
    }
}
