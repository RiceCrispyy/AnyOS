//! Kernel objects and the handle table.
//!
//! Every `HANDLE` the guest holds (other than the console handles, pseudo
//! handles and registry keys) indexes this table. Several handles can refer to
//! one object (`DuplicateHandle`, opening a named object twice); the object
//! goes away with its last handle.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::time::Instant;

use anyos_backend::X86Context;

use crate::file::{FileObj, FindObj, MappingObj, PipeBuf, PipeEnd};
use crate::heap::Heap;
use crate::memory::Reservation;
use crate::process::ChildProcess;
use crate::thread::Thread;
use crate::ui::UiState;

pub(crate) enum Object {
    Event {
        manual: bool,
        signaled: bool,
    },
    Mutex {
        /// Owning thread id and how many times it acquired the mutex.
        owner: Option<(u32, u32)>,
        abandoned: bool,
    },
    Semaphore {
        count: u32,
        max: u32,
    },
    Timer {
        manual: bool,
        due: Option<Instant>,
        period_ms: u32,
        signaled: bool,
    },
    /// Index into `Thread::threads`.
    Thread(usize),
    /// The running process, opened by id.
    SelfProcess,
    Child(ChildProcess),
    /// The primary thread of a child process: signaled when the process ends.
    ChildThread(ChildProcess),
    File(FileObj),
    Find(FindObj),
    Mapping(MappingObj),
    Pipe(PipeEnd),
    /// `NUL`: reads nothing, swallows writes.
    Null,
    /// A fiber (cooperative coroutine).
    Fiber {
        ctx: Box<X86Context>,
        stack_base: u64,
        data: u64,
    },
    /// I/O completion port.
    Iocp {
        queue: VecDeque<(u64, u64, u64)>,
    },
    /// Server end of a named pipe.
    NamedPipeServer {
        name: String,
        c2s: Rc<RefCell<PipeBuf>>,
        s2c: Rc<RefCell<PipeBuf>>,
        connected: bool,
    },
    /// Client end of a named pipe.
    NamedPipeClient {
        c2s: Rc<RefCell<PipeBuf>>,
        s2c: Rc<RefCell<PipeBuf>>,
    },
}

struct Slot {
    obj: Object,
    handles: u32,
    name: Option<String>,
}

/// The process-wide state the Windows personality keeps besides threads.
#[derive(Default)]
pub(crate) struct Kernel {
    slots: Vec<Option<Slot>>,
    handles: BTreeMap<u32, usize>,
    names: HashMap<String, usize>,
    next_handle: u32,
    pub heaps: Vec<Heap>,
    pub process_heap: u64,
    pub reservations: BTreeMap<u64, Reservation>,
    /// Mapped file views: base -> view.
    pub views: BTreeMap<u64, crate::file::View>,
    /// Which dynamic TLS indexes are in use.
    pub tls_used: Vec<bool>,
    /// FLS slots in use, with their callbacks.
    pub fls: Vec<Option<u64>>,
    pub env: Vec<(String, String)>,
    /// The current directory as a Windows path, ending without `\` (except roots).
    pub cwd: String,
    pub std_override: HashMap<u32, u64>,
    pub atoms: Vec<Option<(String, u32)>>,
    pub registry: anyos_env::Registry,
    pub registry_dirty: bool,
    pub reg_keys: HashMap<u32, String>,
    pub next_reg_key: u32,
    pub unhandled_filter: u64,
    pub console_title: String,
    pub console_mode: HashMap<u64, u32>,
    pub thread_locale: u32,
    pub priority_class: u32,
    pub ui: UiState,
    /// Handle of the fiber the current thread is running as (0 = none).
    pub current_fiber: u64,
    /// Named pipes registered by CreateNamedPipe: pipe name -> server handle.
    pub named_pipes: HashMap<String, u64>,
}

/// First handle value handed out. Real Windows hands out multiples of 4; these
/// stay clear of the console handles, the pseudo handles (-1, -2) and
/// `INVALID_HANDLE_VALUE`.
const FIRST_HANDLE: u32 = 0x100;

impl Kernel {
    /// Add `obj` and return a new handle to it.
    pub fn insert(&mut self, obj: Object, name: Option<String>) -> u64 {
        let slot = Slot {
            obj,
            handles: 0,
            name: name.clone(),
        };
        let id = match self.slots.iter().position(Option::is_none) {
            Some(i) => {
                self.slots[i] = Some(slot);
                i
            }
            None => {
                self.slots.push(Some(slot));
                self.slots.len() - 1
            }
        };
        if let Some(n) = name {
            self.names.insert(n, id);
        }
        self.new_handle(id)
    }

    fn new_handle(&mut self, id: usize) -> u64 {
        if self.next_handle < FIRST_HANDLE {
            self.next_handle = FIRST_HANDLE;
        }
        let h = self.next_handle;
        self.next_handle += 4;
        self.handles.insert(h, id);
        if let Some(s) = self.slots[id].as_mut() {
            s.handles += 1;
        }
        h as u64
    }

    /// A new handle to the object `h` refers to.
    pub fn duplicate(&mut self, h: u64) -> Option<u64> {
        let id = *self.handles.get(&(h as u32))?;
        Some(self.new_handle(id))
    }

    /// A new handle to the named object, if one exists.
    pub fn open_named(&mut self, name: &str) -> Option<u64> {
        let id = *self.names.get(name)?;
        Some(self.new_handle(id))
    }

    pub fn named(&mut self, name: &str) -> Option<&mut Object> {
        let id = *self.names.get(name)?;
        self.slots[id].as_mut().map(|s| &mut s.obj)
    }

    pub fn get(&mut self, h: u64) -> Option<&mut Object> {
        let id = *self.handles.get(&(h as u32))?;
        self.slots[id].as_mut().map(|s| &mut s.obj)
    }

    pub fn contains(&self, h: u64) -> bool {
        self.handles.contains_key(&(h as u32))
    }

    /// Close one handle. Returns the object when it was the last handle.
    pub fn close(&mut self, h: u64) -> Option<Option<Object>> {
        let id = self.handles.remove(&(h as u32))?;
        let slot = self.slots[id].as_mut()?;
        slot.handles -= 1;
        if slot.handles > 0 {
            return Some(None);
        }
        let slot = self.slots[id].take()?;
        if let Some(n) = slot.name {
            self.names.remove(&n);
        }
        Some(Some(slot.obj))
    }

    /// Every live object, for state changes that affect many (a thread
    /// ending abandons the mutexes it owns).
    pub fn objects_mut(&mut self) -> impl Iterator<Item = &mut Object> {
        self.slots.iter_mut().flatten().map(|s| &mut s.obj)
    }

    /// Open handles, for diagnostics.
    pub fn handle_count(&self) -> usize {
        self.handles.len()
    }
}

/// The object a handle (including the current-thread pseudo handle) refers to,
/// as far as waiting and thread APIs care.
pub(crate) enum Target {
    Object,
    CurrentThread,
    CurrentProcess,
}

/// Pseudo handles arrive sign-extended on x64 and zero-extended on x86.
pub(crate) fn classify(t: &Thread<'_, '_>, h: u64) -> Target {
    let h32 = h as u32;
    let _ = t;
    match h32 {
        0xFFFF_FFFF => Target::CurrentProcess,
        0xFFFF_FFFE => Target::CurrentThread,
        _ => Target::Object,
    }
}
