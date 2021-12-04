use log::{error, info, trace, warn};
use std::ffi::{c_void, CStr};
use std::mem::MaybeUninit;
use std::os::raw::c_uint;
use std::process::Command;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use x11::keysym::{XK_Tab, XK_space, XK_Q};
use x11::xlib::{
    BadAccess, Button1, Button1Mask, ButtonMotionMask, ButtonPress, ButtonPressMask,
    ButtonReleaseMask, ConfigureNotify, ConfigureRequest, CreateNotify, CurrentTime, DestroyNotify,
    Display, GrabModeAsync, IsViewable, KeyPress, KeyRelease, MapRequest, Mod1Mask, MotionNotify,
    ReparentNotify, RevertToPointerRoot, SubstructureNotifyMask, SubstructureRedirectMask,
    UnmapNotify, Window, XAddToSaveSet, XButtonPressedEvent, XCloseDisplay, XConfigureEvent,
    XConfigureRequestEvent, XConfigureWindow, XCreateSimpleWindow, XCreateWindowEvent,
    XDefaultRootWindow, XDestroyWindow, XDestroyWindowEvent, XDisplayName, XDisplayString,
    XErrorEvent, XFree, XGetGeometry, XGetInputFocus, XGetWindowAttributes, XGrabButton, XGrabKey,
    XGrabServer, XKeyPressedEvent, XKeyReleasedEvent, XKeysymToKeycode, XKillClient,
    XMapRequestEvent, XMapWindow, XMotionEvent, XMoveWindow, XNextEvent, XOpenDisplay, XQueryTree,
    XRaiseWindow, XRemoveFromSaveSet, XReparentEvent, XReparentWindow, XSelectInput,
    XSetErrorHandler, XSetInputFocus, XSync, XUngrabServer, XUnmapEvent, XUnmapWindow,
    XWindowAttributes, XWindowChanges, ButtonRelease, XButtonReleasedEvent,
};

#[derive(Debug)]
struct ClientList(Vec<(Window, Window)>);

impl ClientList {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn contains(&self, w: &Window) -> bool {
        self.0.iter().find(|(win, _)| win == w).is_some()
    }

    pub fn find(&self, w: &Window) -> Option<usize> {
        self.0
            .iter()
            .enumerate()
            .find(|(_, (win, _))| win == w)
            .map(|(i, _)| i)
    }

    pub fn index(&self, i: usize) -> Option<(&Window, &Window)> {
        self.0.get(i).map(|(w, f)| (w, f))
    }

    pub fn get(&self, w: &Window) -> Option<&Window> {
        self.0.iter().find(|(win, _)| win == w).map(|(_, f)| f)
    }

    pub fn insert(&mut self, w: Window, f: Window) {
        self.0.push((w, f));
    }

    pub fn remove(&mut self, w: &Window) {
        if let Some(i) = self.find(w) {
            self.0.remove(i);
        }
    }
}

fn main() {
    stderrlog::new()
        .module(module_path!())
        .verbosity(10)
        .init()
        .unwrap();

    let wm = match WindowManager::new() {
        Some(wm) => wm,
        None => panic!("Failed to initialize window manager"),
    };

    wm.run();
}

pub struct WindowManager {
    display: NonNull<Display>,
    root: Window,
    clients: ClientList,
    drag_pos_start: Option<(i32, i32)>,
    drag_frame_pos: Option<(i32, i32)>,
}

static WM_DETECTED: AtomicBool = AtomicBool::new(false);

impl WindowManager {
    pub fn new() -> Option<Box<WindowManager>> {
        let display = match NonNull::new(unsafe { XOpenDisplay(ptr::null()) }) {
            Some(display) => display,
            None => {
                error!("Failed to open X display: {:?}", unsafe {
                    CStr::from_ptr(XDisplayName(ptr::null()))
                });
                return None;
            }
        };

        let root = unsafe { XDefaultRootWindow(display.as_ptr()) };

        Some(Box::new(WindowManager {
            display,
            root,
            clients: ClientList::new(),
            drag_pos_start: None,
            drag_frame_pos: None,
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
                error!(
                    "Detected another window manager on display {:?}",
                    CStr::from_ptr(XDisplayString(self.display.as_ptr()))
                );
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
            info!(
                "There were {} windows already existing",
                num_top_level_windows
            );
            for i in 0..num_top_level_windows {
                self.frame(ptr::read(top_level_windows.add(i as usize)), true);
            }

            XFree(top_level_windows as *mut c_void);
            XUngrabServer(self.display.as_ptr());
        }

        self.grab_key(Mod1Mask, XK_space, self.root);

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
                ButtonPress => self.on_button_pressed(XButtonPressedEvent::from(e)),
                ButtonRelease => self.on_button_released(XButtonReleasedEvent::from(e)),
                MotionNotify => self.on_motion_notify(XMotionEvent::from(e)),
                KeyPress => self.on_key_pressed(XKeyPressedEvent::from(e)),
                KeyRelease => self.on_key_released(XKeyReleasedEvent::from(e)),
                _ => warn!("Ignored event: {}", e.get_type()),
            }
        }
    }

    fn on_motion_notify(&mut self, e: XMotionEvent) {
        assert!(self.clients.contains(&e.window));
        assert!(self.drag_pos_start.is_some());
        assert!(self.drag_frame_pos.is_some());
        let frame = *self.clients.get(&e.window).unwrap();
        let drag_pos_start = self.drag_pos_start.unwrap();
        let delta = (e.x_root - drag_pos_start.0, e.y_root - drag_pos_start.1);

        if e.state & Button1Mask != 0 {
            let start_frame_pos = self.drag_frame_pos.unwrap();
            let new_frame_pos = (start_frame_pos.0 + delta.0, start_frame_pos.1 + delta.1);
            unsafe {
                XMoveWindow(
                    self.display.as_ptr(),
                    frame,
                    new_frame_pos.0,
                    new_frame_pos.1,
                );
            }
        }
    }

    fn on_button_pressed(&mut self, e: XButtonPressedEvent) {
        assert!(self.clients.contains(&e.window));
        let frame = *self.clients.get(&e.window).unwrap();

        self.drag_pos_start = Some((e.x_root, e.y_root));

        let mut returned_root: Window = 0;
        let mut x: i32 = 0;
        let mut y: i32 = 0;
        let mut width: u32 = 0;
        let mut height: u32 = 0;
        let mut border_width: u32 = 0;
        let mut depth: u32 = 0;
        unsafe {
            XGetGeometry(
                self.display.as_ptr(),
                frame,
                &mut returned_root,
                &mut x,
                &mut y,
                &mut width,
                &mut height,
                &mut border_width,
                &mut depth,
            );
        }
        self.drag_frame_pos = Some((x, y));

        unsafe {
            XRaiseWindow(self.display.as_ptr(), frame);
            XSetInputFocus(
                self.display.as_ptr(),
                e.window,
                RevertToPointerRoot,
                CurrentTime,
            );
        }
    }

    fn on_button_released(&mut self, _e: XButtonReleasedEvent) {
        self.drag_frame_pos = None;
        self.drag_pos_start = None;
    }

    fn on_key_pressed(&mut self, e: XKeyPressedEvent) {
        info!("key pressed: {}", e.keycode);
        let mut w = 0;
        let mut focus_state = 0;
        unsafe {
            XGetInputFocus(self.display.as_ptr(), &mut w, &mut focus_state);
        }
        trace!("current focused window: {}", w);
        trace!("event window: {}", e.window);
        trace!("root window: {}", self.root);
        if e.keycode == unsafe { XKeysymToKeycode(self.display.as_ptr(), XK_Q.into()) }.into() {
            // Kill client
            info!("Killing window {}", e.window);
            unsafe {
                XKillClient(self.display.as_ptr(), e.window);
            }
        } else if e.state & Mod1Mask != 0
            && e.keycode == unsafe { XKeysymToKeycode(self.display.as_ptr(), XK_Tab.into()) }.into()
        {
            trace!("clients: {:?}", self.clients);
            let mut w = 0;
            let mut focus_state = 0;
            unsafe {
                XGetInputFocus(self.display.as_ptr(), &mut w, &mut focus_state);
            }
            trace!("current focused window: {}", w);
            trace!("event window: {}", e.window);
            trace!("root window: {}", self.root);
            let i = self.clients.find(&e.window).unwrap();
            let i = (i + 1) % self.clients.len();
            let (&w, &f) = self.clients.index(i).unwrap();

            unsafe {
                XRaiseWindow(self.display.as_ptr(), f);
                XSetInputFocus(self.display.as_ptr(), w, RevertToPointerRoot, CurrentTime);
            }
        } else if e.state & Mod1Mask != 0
            && e.keycode
                == unsafe { XKeysymToKeycode(self.display.as_ptr(), XK_space.into()) }.into()
        {
            Command::new("/home/ole/dotfiles/bin/dmenu_run_history")
                .spawn()
                .unwrap();
        }
    }

    fn on_key_released(&mut self, e: XKeyReleasedEvent) {
        info!("key released: {}", e.keycode);
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
            XAddToSaveSet(display, w);
            XReparentWindow(display, w, frame, 0, 0);
            XMapWindow(display, frame);
            self.clients.insert(w, frame);

            // grab events
            self.grab_key(Mod1Mask, XK_Q, w);
            self.grab_key(Mod1Mask, XK_Tab, w);
            self.grab_button(Mod1Mask, Button1, w);

            trace!("Framed window {} [{}]", w, frame);
        }
    }

    fn grab_button(&self, modifiers: c_uint, button: c_uint, w: Window) {
        unsafe {
            XGrabButton(
                self.display.as_ptr(),
                button,
                modifiers,
                w,
                0,
                (ButtonPressMask | ButtonReleaseMask | ButtonMotionMask)
                    .try_into()
                    .unwrap(),
                GrabModeAsync,
                GrabModeAsync,
                0,
                0,
            );
        }
    }

    fn grab_key(&self, modifiers: c_uint, key_code: c_uint, w: Window) {
        unsafe {
            XGrabKey(
                self.display.as_ptr(),
                XKeysymToKeycode(self.display.as_ptr(), key_code.into()).into(),
                modifiers,
                w,
                0,
                GrabModeAsync,
                GrabModeAsync,
            );
        }
    }

    fn on_map_request(&mut self, e: XMapRequestEvent) {
        self.frame(e.window, false);

        unsafe {
            XMapWindow(self.display.as_ptr(), e.window);
            trace!("Mapped window {}", e.window);
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

            trace!("Unframed window {} [{}]", w, frame);
        }
    }

    fn on_unmap_notify(&mut self, e: XUnmapEvent) {
        if e.event != self.root && self.clients.contains(&e.window) {
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

            trace!("Configured window {}", e.window);
        }
    }

    fn on_configure_notify(&mut self, _e: XConfigureEvent) {}

    fn on_create_notify(&mut self, e: XCreateWindowEvent) {
        trace!("Window {} created", e.window);
    }

    fn on_destroy_notify(&mut self, e: XDestroyWindowEvent) {
        trace!("Window {} destroyed", e.window);
    }

    fn on_reparent_notify(&mut self, e: XReparentEvent) {
        trace!("Window {} reparented", e.window);
    }

    extern "C" fn on_x_error(_: *mut Display, e: *mut XErrorEvent) -> i32 {
        let e = unsafe { &*e };
        error!("X Error: {:?}", e);

        0
    }
    extern "C" fn on_wm_detected(_: *mut Display, e: *mut XErrorEvent) -> i32 {
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
