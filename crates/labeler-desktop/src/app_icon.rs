//! The pinned AntiWebUI backend does not expose window icons yet.
//! Load our EXE resource and set icons on our own main window only.
#[cfg(windows)]
pub fn install() {
    use std::{ffi::c_void, time::Duration};
    type Handle = *mut c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> Handle;
        fn GetCurrentProcessId() -> u32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(
            callback: unsafe extern "system" fn(Handle, isize) -> i32,
            data: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(window: Handle, process: *mut u32) -> u32;
        fn GetWindowTextW(window: Handle, text: *mut u16, count: i32) -> i32;
        fn LoadImageW(
            module: Handle,
            name: *const u16,
            kind: u32,
            x: i32,
            y: i32,
            flags: u32,
        ) -> Handle;
        fn SendMessageTimeoutW(
            window: Handle,
            message: u32,
            wparam: usize,
            lparam: isize,
            flags: u32,
            timeout: u32,
            result: *mut usize,
        ) -> isize;
    }
    unsafe extern "system" fn find(window: Handle, data: isize) -> i32 {
        let mut process = 0;
        unsafe { GetWindowThreadProcessId(window, &mut process) };
        if process != unsafe { GetCurrentProcessId() } {
            return 1;
        }
        let mut title = [0u16; 128];
        let len = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
        if String::from_utf16_lossy(&title[..len.max(0) as usize]) != "YOLO Labeler — AntiWebUI" {
            return 1;
        }
        let module = unsafe { GetModuleHandleW(std::ptr::null()) };
        for (slot, size) in [(0, 32), (1, 64)] {
            // MAKEINTRESOURCE(1), IMAGE_ICON, LR_SHARED: module owns the lifetime.
            let icon = unsafe { LoadImageW(module, 1usize as *const u16, 1, size, size, 0x8000) };
            if !icon.is_null() {
                unsafe {
                    SendMessageTimeoutW(
                        window,
                        0x80,
                        slot,
                        icon as isize,
                        2,
                        2000,
                        std::ptr::null_mut(),
                    )
                };
            }
        }
        unsafe { *(data as *mut bool) = true };
        0
    }
    std::thread::spawn(|| {
        for _ in 0..200 {
            let mut found = false;
            unsafe { EnumWindows(find, &mut found as *mut bool as isize) };
            if found {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

#[cfg(not(windows))]
pub fn install() {}
