//! The desktop's player for the music tab: libmpv, loaded when first needed so the app runs (and
//! builds) without it. One thread owns it, with no video at all.

use std::{
    ffi::{CStr, CString, c_char, c_double, c_int, c_void},
    sync::{Arc, Mutex, OnceLock, mpsc},
    time::Instant,
};

use crate::player::Update;

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
}

const EVENT_SHUTDOWN: c_int = 1;
const EVENT_START_FILE: c_int = 6;
const EVENT_END_FILE: c_int = 7;
const EVENT_FILE_LOADED: c_int = 8;
const EVENT_PROPERTY_CHANGE: c_int = 22;
const FORMAT_STRING: c_int = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_DOUBLE: c_int = 5;
const END_FILE_EOF: c_int = 0;
const END_FILE_ERROR: c_int = 4;

type Handle = *mut c_void;
type Callback = unsafe extern "C" fn(*mut c_void);

/// The few libmpv functions the player calls.
struct Lib {
    _lib: libloading::Library,
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    set_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    observe_property: unsafe extern "C" fn(Handle, u64, *const c_char, c_int) -> c_int,
    command: unsafe extern "C" fn(Handle, *mut *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(Handle, c_double) -> *mut MpvEvent,
    set_wakeup_callback: unsafe extern "C" fn(Handle, Option<Callback>, *mut c_void),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    terminate_destroy: unsafe extern "C" fn(Handle),
}

const LIB_NAMES: &[&str] = &[
    "libmpv.so.2",
    "libmpv.so",
    "libmpv.2.dylib",
    "libmpv.dylib",
    "/opt/homebrew/lib/libmpv.dylib",
    "/usr/local/lib/libmpv.dylib",
    "libmpv-2.dll",
    "mpv-2.dll",
];

impl Lib {
    fn load() -> Result<Lib, String> {
        let lib = LIB_NAMES
            .iter()
            .find_map(|name| unsafe { libloading::Library::new(name) }.ok())
            .ok_or_else(|| "Playing needs mpv (libmpv), which is not installed.".to_owned())?;
        macro_rules! sym {
            ($name:literal) => {
                *unsafe { lib.get(concat!($name, "\0").as_bytes()) }
                    .map_err(|e| format!("this libmpv is too old: {e}"))?
            };
        }
        Ok(Lib {
            create: sym!("mpv_create"),
            initialize: sym!("mpv_initialize"),
            set_option_string: sym!("mpv_set_option_string"),
            set_property: sym!("mpv_set_property"),
            observe_property: sym!("mpv_observe_property"),
            command: sym!("mpv_command"),
            wait_event: sym!("mpv_wait_event"),
            set_wakeup_callback: sym!("mpv_set_wakeup_callback"),
            error_string: sym!("mpv_error_string"),
            terminate_destroy: sym!("mpv_terminate_destroy"),
            _lib: lib,
        })
    }

    fn error(&self, code: c_int) -> String {
        unsafe { CStr::from_ptr((self.error_string)(code)) }
            .to_string_lossy()
            .into_owned()
    }
}

fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(Lib::load).as_ref().map_err(Clone::clone)
}

/// What wakes the player's thread.
enum Wake {
    /// A ticket, the source, and where to start (ms).
    Load(u64, String, u64),
    Seek(u64),
    Pause(bool),
    Stop,
    /// mpv has events.
    Events,
}

unsafe extern "C" fn wake_events(tx: *mut c_void) {
    let _ = unsafe { &*(tx as *const mpsc::Sender<Wake>) }.send(Wake::Events);
}

/// What the player tells: which load it is about (its ticket), and what happened.
pub type Told = Box<dyn Fn(u64, Update) + Send>;

/// The player. Cheap to clone; every clone drives the same one.
#[derive(Clone)]
pub struct Mpv {
    tx: mpsc::Sender<Wake>,
    clock: Arc<Mutex<Clock>>,
}

/// Where the song was when mpv last said, so where it is now can be told without asking it.
#[derive(Default)]
struct Clock {
    /// Seconds into the song, and when that was; none before it plays.
    at: Option<(f64, Instant)>,
    paused: bool,
}

impl Mpv {
    /// Start the player, if libmpv is there.
    pub fn start(told: Told) -> Result<Mpv, String> {
        let lib = lib()?;
        let (tx, rx) = mpsc::channel();
        let wake = tx.clone();
        let clock = Arc::new(Mutex::new(Clock::default()));
        let told_clock = clock.clone();
        // The thread says whether mpv started.
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("mpv".into())
            .spawn(move || {
                // What mpv's callback gets, freed with the worker.
                let wake = Box::into_raw(Box::new(wake));
                let worker = match Worker::new(lib, wake, told, told_clock) {
                    Ok(worker) => worker,
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                worker.run(rx);
            })
            .map_err(|e| e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Mpv { tx, clock })
    }

    /// Play `source` (a path or an address) from `start_ms` on; what it does is told with `ticket`.
    pub fn load(&self, ticket: u64, source: String, start_ms: u64) {
        self.clock.lock().unwrap().at = None;
        let _ = self.tx.send(Wake::Load(ticket, source, start_ms));
    }

    pub fn seek(&self, ms: u64) {
        let _ = self.tx.send(Wake::Seek(ms));
    }

    /// Where the song plays, in ms; none before it plays.
    pub fn position(&self) -> Option<u64> {
        let clock = self.clock.lock().unwrap();
        let (secs, when) = clock.at?;
        let ran = if clock.paused {
            0.
        } else {
            when.elapsed().as_secs_f64()
        };
        Some(((secs + ran) * 1000.) as u64)
    }

    pub fn pause(&self, paused: bool) {
        let _ = self.tx.send(Wake::Pause(paused));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(Wake::Stop);
    }
}

struct Worker {
    lib: &'static Lib,
    mpv: Handle,
    /// The sender the callback uses, freed after mpv is gone.
    wake: *mut mpsc::Sender<Wake>,
    told: Told,
    clock: Arc<Mutex<Clock>>,
}

impl Worker {
    fn new(
        lib: &'static Lib,
        wake: *mut mpsc::Sender<Wake>,
        told: Told,
        clock: Arc<Mutex<Clock>>,
    ) -> Result<Worker, String> {
        let mpv = unsafe { (lib.create)() };
        if mpv.is_null() {
            drop(unsafe { Box::from_raw(wake) });
            return Err("mpv could not start".into());
        }
        let worker = Worker {
            lib,
            mpv,
            wake,
            told,
            clock,
        };
        for (name, value) in [
            ("video", "no"),
            ("vo", "null"),
            ("audio-display", "no"),
            ("idle", "yes"),
            ("keep-open", "no"),
            ("ytdl", "no"),
            ("input-default-bindings", "no"),
            ("osc", "no"),
        ] {
            let (name, value) = (CString::new(name).unwrap(), CString::new(value).unwrap());
            unsafe { (lib.set_option_string)(mpv, name.as_ptr(), value.as_ptr()) };
        }
        let code = unsafe { (lib.initialize)(mpv) };
        if code < 0 {
            return Err(format!("mpv could not start: {}", lib.error(code)));
        }
        unsafe {
            (lib.observe_property)(mpv, 0, c"pause".as_ptr(), FORMAT_FLAG);
            (lib.observe_property)(mpv, 0, c"time-pos".as_ptr(), FORMAT_DOUBLE);
            (lib.set_wakeup_callback)(mpv, Some(wake_events), wake as *mut c_void);
        }
        Ok(worker)
    }

    fn run(self, rx: mpsc::Receiver<Wake>) {
        // The tickets of the file mpv plays and of the latest one asked for: what a file says about
        // its end is only news while it is the latest.
        let (mut playing, mut loading) = (0, 0);
        while let Ok(wake) = rx.recv() {
            match wake {
                Wake::Load(ticket, source, start_ms) => {
                    loading = ticket;
                    // An option for the next file only: mpv resets it when the file ends.
                    let start = if start_ms > 0 {
                        seconds(start_ms)
                    } else {
                        "none".into()
                    };
                    self.set_string("start", &start);
                    self.command(ticket, &["loadfile", &source, "replace"]);
                    self.set_flag("pause", false);
                }
                Wake::Pause(paused) => self.set_flag("pause", paused),
                // A seek it cannot do is no reason to drop the song: no ticket to fail.
                Wake::Seek(ms) => self.command(0, &["seek", &seconds(ms), "absolute+exact"]),
                Wake::Stop => {
                    (loading, playing) = (0, 0);
                    self.command(0, &["stop"]);
                }
                Wake::Events => {
                    if !self.events(&mut playing, loading) {
                        return;
                    }
                }
            }
        }
    }

    /// Handle what mpv has to say; false once it shut down.
    fn events(&self, playing: &mut u64, loading: u64) -> bool {
        loop {
            let event = unsafe { &*(self.lib.wait_event)(self.mpv, 0.) };
            match event.event_id {
                0 => return true,
                EVENT_SHUTDOWN => return false,
                EVENT_START_FILE => *playing = loading,
                EVENT_FILE_LOADED => (self.told)(*playing, Update::Loaded),
                EVENT_END_FILE if !event.data.is_null() => {
                    let end = unsafe { &*(event.data as *const MpvEventEndFile) };
                    // Replaced or stopped: whoever did it knows.
                    if *playing == 0 || *playing != loading {
                        continue;
                    }
                    let ticket = std::mem::take(playing);
                    match end.reason {
                        END_FILE_EOF => (self.told)(ticket, Update::Ended),
                        END_FILE_ERROR => (self.told)(
                            ticket,
                            Update::Failed(format!(
                                "could not play it: {}",
                                self.lib.error(end.error)
                            )),
                        ),
                        _ => {}
                    }
                }
                EVENT_PROPERTY_CHANGE if !event.data.is_null() => {
                    let prop = unsafe { &*(event.data as *const MpvEventProperty) };
                    let name = unsafe { CStr::from_ptr(prop.name) };
                    if name == c"pause" && prop.format == FORMAT_FLAG && !prop.data.is_null() {
                        let paused = unsafe { *(prop.data as *const c_int) } != 0;
                        let mut clock = self.clock.lock().unwrap();
                        // Keep where it was when it paused.
                        if let Some((secs, when)) = clock.at
                            && !clock.paused
                        {
                            clock.at = Some((secs + when.elapsed().as_secs_f64(), Instant::now()));
                        }
                        clock.paused = paused;
                        drop(clock);
                        (self.told)(loading, Update::Paused(paused));
                    } else if name == c"time-pos" {
                        let secs = (prop.format == FORMAT_DOUBLE && !prop.data.is_null())
                            .then(|| unsafe { *(prop.data as *const c_double) });
                        self.clock.lock().unwrap().at = secs.map(|s| (s, Instant::now()));
                    }
                }
                _ => {}
            }
        }
    }

    fn command(&self, ticket: u64, args: &[&str]) {
        let args: Vec<CString> = args.iter().filter_map(|a| CString::new(*a).ok()).collect();
        let mut ptrs: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        let code = unsafe { (self.lib.command)(self.mpv, ptrs.as_mut_ptr()) };
        if code < 0 {
            (self.told)(ticket, Update::Failed(self.lib.error(code)));
        }
    }

    fn set_string(&self, name: &str, value: &str) {
        let (name, value) = (CString::new(name).unwrap(), CString::new(value).unwrap());
        let mut ptr = value.as_ptr();
        unsafe {
            (self.lib.set_property)(
                self.mpv,
                name.as_ptr(),
                FORMAT_STRING,
                &mut ptr as *mut *const c_char as *mut c_void,
            )
        };
    }

    fn set_flag(&self, name: &str, on: bool) {
        let name = CString::new(name).unwrap();
        let mut value: c_int = on.into();
        unsafe {
            (self.lib.set_property)(
                self.mpv,
                name.as_ptr(),
                FORMAT_FLAG,
                &mut value as *mut c_int as *mut c_void,
            )
        };
    }
}

/// `ms` as mpv takes times: seconds.
fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

impl Drop for Worker {
    fn drop(&mut self) {
        unsafe {
            (self.lib.set_wakeup_callback)(self.mpv, None, std::ptr::null_mut());
            (self.lib.terminate_destroy)(self.mpv);
            drop(Box::from_raw(self.wake));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Plays the file in `FLIPFLOP_TEST_MEDIA` to its end (needs libmpv):
    /// `cargo test -p flipflop-app mpv -- --ignored`.
    #[test]
    #[ignore]
    fn plays_with_mpv() {
        let path = std::env::var("FLIPFLOP_TEST_MEDIA").expect("FLIPFLOP_TEST_MEDIA");
        let (tx, rx) = mpsc::channel();
        let player = Mpv::start(Box::new(move |ticket, update| {
            let _ = tx.send((ticket, update));
        }))
        .unwrap();
        player.load(7, path, 0);
        let mut loaded = false;
        loop {
            match rx
                .recv_timeout(Duration::from_secs(20))
                .expect("did not end")
            {
                (7, Update::Loaded) => loaded = true,
                (7, Update::Ended) => break,
                (_, Update::Failed(why)) => panic!("{why}"),
                _ => {}
            }
        }
        assert!(loaded);
    }
}
