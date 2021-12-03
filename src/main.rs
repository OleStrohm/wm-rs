#![allow(unused)]
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::mem::MaybeUninit;
use std::ops::Add;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use x11::xlib::{
    BadAccess, ConfigureNotify, ConfigureRequest, CreateNotify, DestroyNotify, Display, IsViewable,
    MapRequest, ReparentNotify, SubstructureNotifyMask, SubstructureRedirectMask, UnmapNotify,
    Window, XCloseDisplay, XConfigureEvent, XConfigureRequestEvent, XConfigureWindow,
    XCreateSimpleWindow, XCreateWindowEvent, XDefaultRootWindow, XDestroyWindow,
    XDestroyWindowEvent, XDisplayName, XDisplayString, XErrorEvent, XEvent, XFree,
    XGetWindowAttributes, XGrabServer, XMapRequestEvent, XMapWindow, XNextEvent, XOpenDisplay,
    XQueryTree, XRemoveFromSaveSet, XReparentEvent, XReparentWindow, XSelectInput,
    XSetErrorHandler, XSync, XUngrabServer, XUnmapEvent, XUnmapWindow, XWindowAttributes,
    XWindowChanges,
};

fn main() {
    let wm = match WindowManager::new() {
        Some(wm) => wm,
        None => panic!("Failed to initialize window manager"),
    };

    wm.run();
}

pub struct WindowManager {
    display: NonNull<Display>,
    root: Window,
    clients: HashMap<Window, Window>,
}

static WM_DETECTED: AtomicBool = AtomicBool::new(false);

impl WindowManager {
    pub fn new() -> Option<Box<WindowManager>> {
        let display = match NonNull::new(unsafe { XOpenDisplay(ptr::null()) }) {
            Some(display) => display,
            None => {
                eprintln!("Failed to open X display: {:?}", unsafe {
                    CStr::from_ptr(XDisplayName(ptr::null()))
                });
                return None;
            }
        };

        let root = unsafe { XDefaultRootWindow(display.as_ptr()) };

        Some(Box::new(WindowManager {
            display,
            root,
            clients: HashMap::new(),
        }))
    }

    pub fn run(mut self) {
        WM_DETECTED.store(false, Ordering::Relaxed);

        unsafe {
            XSetErrorHandler(Some(WindowManager::on_wm_detected));
            XSelectInput(
                self.display.as_ptr(),
                self.root,
                SubstructureRedirectMask | SubstructureNotifyMask,
            );

            XSync(self.display.as_ptr(), 0);

            if WM_DETECTED.load(Ordering::Relaxed) {
                eprintln!("Detected another window manager on display {:?}", unsafe {
                    CStr::from_ptr(XDisplayString(self.display.as_ptr()))
                });
                return;
            }

            XSetErrorHandler(Some(WindowManager::on_x_error));
        }

        unsafe {
            XGrabServer(self.display.as_ptr());
        }
        let mut returned_root = 0;
        let mut returned_parent = 0;
        let mut top_level_windows: *mut u64 = std::ptr::null_mut();
        let mut num_top_level_windows = 0;

        let status = unsafe {
            XQueryTree(
                self.display.as_ptr(),
                self.root,
                &mut returned_root,
                &mut returned_parent,
                &mut top_level_windows,
                &mut num_top_level_windows,
            )
        };
        assert_ne!(status, 0);
        assert_eq!(returned_root, self.root);

        unsafe {
            for i in 0..num_top_level_windows {
                self.frame(ptr::read(top_level_windows.add(i as usize)), true);
            }

            XFree(top_level_windows as *mut c_void);
            XUngrabServer(self.display.as_ptr());
        }

        loop {
            let e = unsafe {
                let mut e = MaybeUninit::uninit();
                XNextEvent(self.display.as_ptr(), e.as_mut_ptr());
                e.assume_init()
            };

            #[allow(non_upper_case_globals)]
            match e.get_type() {
                ConfigureRequest => self.on_configure_request(XConfigureRequestEvent::from(e)),
                ConfigureNotify => self.on_configure_notify(XConfigureEvent::from(e)),
                MapRequest => self.on_map_request(XMapRequestEvent::from(e)),
                UnmapNotify => self.on_unmap_notify(XUnmapEvent::from(e)),
                CreateNotify => self.on_create_notify(XCreateWindowEvent::from(e)),
                DestroyNotify => self.on_destroy_notify(XDestroyWindowEvent::from(e)),
                ReparentNotify => self.on_reparent_notify(XReparentEvent::from(e)),
                _ => eprintln!("Ignored event: {}", e.get_type()),
            }
        }
    }

    fn frame(&mut self, w: Window, created_before_wm: bool) {
        const BORDER_WIDTH: u32 = 3;
        const BORDER_COLOR: u64 = 0xFF00FF;
        const BG_COLOR: u64 = 0x0000FF;

        let display = self.display.as_ptr();

        let attributes: XWindowAttributes = unsafe {
            let mut attributes = MaybeUninit::uninit();
            let status = XGetWindowAttributes(display, w, attributes.as_mut_ptr());
            assert_ne!(status, 0);
            attributes.assume_init()
        };

        if created_before_wm
            && (attributes.override_redirect != 0 || attributes.map_state != IsViewable)
        {
            return;
        }

        unsafe {
            let frame = XCreateSimpleWindow(
                display,
                self.root,
                attributes.x,
                attributes.y,
                attributes.width.try_into().unwrap(),
                attributes.height.try_into().unwrap(),
                BORDER_WIDTH,
                BORDER_COLOR,
                BG_COLOR,
            );

            XSelectInput(
                display,
                frame,
                SubstructureRedirectMask | SubstructureNotifyMask,
            );
            XReparentWindow(self.display.as_ptr(), w, frame, 0, 0);
            XMapWindow(display, frame);
            self.clients.insert(w, frame);
            // grab events
        }
    }

    fn on_map_request(&mut self, e: XMapRequestEvent) {
        self.frame(e.window, false);

        unsafe {
            XMapWindow(self.display.as_ptr(), e.window);
        }
    }

    fn unframe(&mut self, w: Window) {
        let frame = *self.clients.get(&w).unwrap();

        unsafe {
            XUnmapWindow(self.display.as_ptr(), frame);
            XReparentWindow(self.display.as_ptr(), w, self.root, 0, 0);
            XRemoveFromSaveSet(self.display.as_ptr(), w);
            XDestroyWindow(self.display.as_ptr(), frame);
            self.clients.remove(&w);
        }
    }

    fn on_unmap_notify(&mut self, e: XUnmapEvent) {
        if e.event != self.root && self.clients.contains_key(&e.window) {
            self.unframe(e.window);
        }
    }

    fn on_configure_request(&mut self, e: XConfigureRequestEvent) {
        let mut changes = XWindowChanges {
            x: e.x,
            y: e.y,
            width: e.width,
            height: e.height,
            border_width: e.border_width,
            sibling: e.above,
            stack_mode: e.detail,
        };

        if let Some(&frame) = self.clients.get(&e.window) {
            unsafe {
                XConfigureWindow(
                    self.display.as_ptr(),
                    frame,
                    e.value_mask.try_into().unwrap(),
                    &mut changes,
                );
            }
        }

        unsafe {
            XConfigureWindow(
                self.display.as_ptr(),
                e.window,
                e.value_mask.try_into().unwrap(),
                &mut changes,
            );
        }
    }

    fn on_configure_notify(&mut self, e: XConfigureEvent) {}

    fn on_create_notify(&mut self, e: XCreateWindowEvent) {}

    fn on_destroy_notify(&mut self, e: XDestroyWindowEvent) {}

    fn on_reparent_notify(&mut self, e: XReparentEvent) {}

    extern "C" fn on_x_error(display: *mut Display, e: *mut XErrorEvent) -> i32 {
        let e = unsafe { &*e };
        eprintln!("X Error: {:?}", e);

        0
    }
    extern "C" fn on_wm_detected(display: *mut Display, e: *mut XErrorEvent) -> i32 {
        assert_eq!(unsafe { (&*e).error_code }, BadAccess);

        WM_DETECTED.store(true, Ordering::Relaxed);

        0
    }
}

impl Drop for WindowManager {
    fn drop(&mut self) {
        unsafe { XCloseDisplay(self.display.as_ptr()) };
    }
}
