#!allow(unsafe_code);

mod scheduler;
mod timer;

use crate::wren;
use std::collections::HashMap;
use std::ffi::CString;

pub struct Class {
    pub methods: HashMap<String, wren::ForeignMethod>,
    pub static_methods: HashMap<String, wren::ForeignMethod>,
}

impl Class {
    fn new() -> Self {
        Self {
            methods: HashMap::new(),
            static_methods: HashMap::new(),
        }
    }
}

pub struct Module {
    pub source: CString,
    pub classes: HashMap<String, Class>,
}

impl Module {
    fn new(source: CString) -> Self {
        Self {
            source,
            classes: HashMap::new(),
        }
    }
}

fn modules_init() -> HashMap<&'static str, Module> {
    let mut m = HashMap::new();
    let scheduler_source = include_str!("scheduler.wren");

    let mut scheduler_class = Class::new();
    scheduler_class
        .static_methods
        .insert("captureMethods_()".to_string(), scheduler::capture_methods);

    let mut scheduler_module = Module::new(CString::new(scheduler_source).unwrap());
    scheduler_module
        .classes
        .insert("Scheduler".to_string(), scheduler_class);

    m.insert("scheduler", scheduler_module);

    let timer_source = include_str!("timer.wren");

    let mut timer_class = Class::new();
    timer_class
        .static_methods
        .insert("startTimer_(_,_)".to_string(), timer::start);

    let mut timer_module = Module::new(CString::new(timer_source).unwrap());
    timer_module
        .classes
        .insert("Timer".to_string(), timer_class);
    m.insert("timer", timer_module);

    m
}

lazy_static! {
    // TODO: Refactor to make this not require modules to stay in memory indefinitely
    static ref MODULES: HashMap<&'static str, Module> = {
        modules_init()
    };
}

pub fn get_module<S>(name: S) -> Option<&'static Module>
where
    S: AsRef<str>,
{
    MODULES.get(name.as_ref())
}
