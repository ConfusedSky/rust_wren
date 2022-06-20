#![allow(unsafe_code)]

mod wren_value;

use std::{
    alloc::Layout,
    borrow::Cow,
    cell::RefCell,
    ffi::{c_void, CStr, CString},
    mem::{transmute_copy, MaybeUninit},
    pin::Pin,
    ptr::{null, NonNull},
};

use wren_sys::{
    self, wrenCall, wrenFreeVM, wrenGetUserData, wrenGetVariable, wrenInitConfiguration,
    wrenInsertInList, wrenInterpret, wrenMakeCallHandle, wrenNewVM, wrenReleaseHandle,
    WrenConfiguration, WrenErrorType, WrenHandle, WrenInterpretResult, WrenLoadModuleResult,
    WrenVM,
};

pub type ForeignMethod = unsafe fn(vm: VMPtr);

unsafe fn get_user_data<'s, V>(vm: *mut WrenVM) -> Option<&'s mut V> {
    let user_data = wrenGetUserData(vm);
    if user_data.is_null() {
        None
    } else {
        Some(user_data.cast::<V>().as_mut().unwrap())
    }
}

// Allow custom logic later this is just for testing for now
// const extern functions aren't stable so this should be ignored
#[allow(clippy::missing_const_for_fn)]
unsafe extern "C" fn resolve_module<V: VmUserData>(
    vm: *mut WrenVM,
    resolver: *const i8,
    name: *const i8,
) -> *const i8 {
    let user_data = get_user_data::<V>(vm);

    user_data.map_or_else(
        || std::mem::zeroed(),
        |user_data| {
            let name = CStr::from_ptr(name).to_string_lossy();
            let resolver = CStr::from_ptr(resolver).to_string_lossy();

            let name = user_data.resolve_module(resolver.as_ref(), name.as_ref());

            match name {
                Some(name) => name.into_raw(),
                None => null(),
            }
        },
    )
}

unsafe extern "C" fn load_module<V: VmUserData>(
    vm: *mut WrenVM,
    name: *const i8,
) -> wren_sys::WrenLoadModuleResult {
    let user_data = get_user_data::<V>(vm);

    user_data.map_or_else(
        || std::mem::zeroed(),
        |user_data| {
            unsafe extern "C" fn cleanup(
                _vm: *mut WrenVM,
                _name: *const i8,
                result: WrenLoadModuleResult,
            ) {
                // Deallocate the slice directly, because calling from raw requires
                // calling strlen which is much slower than just calling dealloc
                std::alloc::dealloc(result.source as *mut u8, Layout::new::<CString>());
            }

            let name = CStr::from_ptr(name).to_string_lossy();

            let source = user_data.load_module(name.as_ref());

            let mut result: wren_sys::WrenLoadModuleResult = std::mem::zeroed();

            if let Some(source) = source {
                // SAFETY: we use into raw here and pass in a function that frees the memory
                result.source = source.into_raw();
                result.onComplete = Some(cleanup);
            }

            result
        },
    )
}

unsafe extern "C" fn bind_foreign_method<V: VmUserData>(
    vm: *mut WrenVM,
    module: *const i8,
    class_name: *const i8,
    is_static: bool,
    signature: *const i8,
) -> wren_sys::WrenForeignMethodFn {
    let user_data = get_user_data::<V>(vm);

    user_data.map_or_else(
        || std::mem::zeroed(),
        |user_data| {
            let module = CStr::from_ptr(module).to_string_lossy();
            let class_name = CStr::from_ptr(class_name).to_string_lossy();
            let signature = CStr::from_ptr(signature).to_string_lossy();

            let method = user_data.bind_foreign_method(
                module.as_ref(),
                class_name.as_ref(),
                is_static,
                signature.as_ref(),
            )?;

            // Safety: VMPtr is a transparent wrapper over a *mut WrenVM
            transmute_copy(&method)
        },
    )
}

unsafe extern "C" fn write_fn<V: VmUserData>(vm: *mut WrenVM, text: *const i8) {
    let user_data = get_user_data::<V>(vm);

    if let Some(user_data) = user_data {
        let text = CStr::from_ptr(text).to_string_lossy();
        user_data.on_write(VMPtr::new_unchecked(vm), text.as_ref());
    }
}

unsafe extern "C" fn error_fn<V: VmUserData>(
    vm: *mut WrenVM,
    error_type: WrenErrorType,
    module: *const i8,
    line: i32,
    msg: *const i8,
) {
    let user_data = get_user_data::<V>(vm);
    if let Some(user_data) = user_data {
        let msg = CStr::from_ptr(msg).to_string_lossy();
        // This lives outside of the if statement so that it can live long enough
        // to be passed to user_data on error
        let c_module: Cow<str>;
        // Runtime doesn't have a valid module so it will crash if it goes any further
        let kind = if error_type == wren_sys::WrenErrorType_WREN_ERROR_RUNTIME {
            ErrorKind::Runtime(msg.as_ref())
        } else {
            c_module = CStr::from_ptr(module).to_string_lossy();
            let context = ErrorContext {
                module: c_module.as_ref(),
                line,
                msg: msg.as_ref(),
            };
            match error_type {
                wren_sys::WrenErrorType_WREN_ERROR_COMPILE => ErrorKind::Compile(context),
                wren_sys::WrenErrorType_WREN_ERROR_RUNTIME => ErrorKind::Runtime(msg.as_ref()),
                wren_sys::WrenErrorType_WREN_ERROR_STACK_TRACE => ErrorKind::Stacktrace(context),
                kind => ErrorKind::Unknown(kind, context),
            }
        };

        user_data.on_error(VMPtr::new_unchecked(vm), kind);
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct VMPtr(NonNull<WrenVM>);

// Ensure that VMPtr is the same Size as `*mut WrenVM`
// the whole purpose of it is to make it easier to access
// the wren api, without having to sacrifice size, performance or ergonomics
// So they should be directly castable
static_assertions::assert_eq_align!(VMPtr, *mut WrenVM);
static_assertions::assert_eq_size!(VMPtr, *mut WrenVM);

type Slot = std::os::raw::c_int;
type Handle = NonNull<WrenHandle>;

impl VMPtr {
    pub const unsafe fn new_unchecked(vm: *mut WrenVM) -> Self {
        Self(NonNull::new_unchecked(vm))
    }

    /// SAFETY: This is not guarenteed to be safe the user needs to know to input
    /// the correct type
    pub unsafe fn get_user_data<'s, V: VmUserData>(self) -> Option<&'s mut V> {
        get_user_data(self.0.as_ptr())
    }

    /// SAFETY: Will segfault if an invalid slot
    /// is asked for
    pub unsafe fn set_slot_handle_unchecked(self, slot: Slot, handle: Handle) {
        wren_sys::wrenSetSlotHandle(self.0.as_ptr(), slot, handle.as_ptr());
    }

    /// SAFETY: Will segfault if an invalid slot
    /// is asked for
    pub unsafe fn set_slot_new_list_unchecked(self, slot: Slot) {
        wren_sys::wrenSetSlotNewList(self.0.as_ptr(), slot);
    }

    pub unsafe fn insert_in_list(self, list_slot: Slot, index: i32, element_slot: Slot) {
        wrenInsertInList(self.0.as_ptr(), list_slot, index, element_slot);
    }

    /// SAFETY: Will segfault if an invalid slot
    /// is set for
    pub unsafe fn set_slot_string_unchecked<S>(self, slot: Slot, value: S)
    where
        S: AsRef<str>,
    {
        let text = CString::new(value.as_ref()).unwrap();
        wren_sys::wrenSetSlotString(self.0.as_ptr(), slot, text.as_ptr());
    }

    /// SAFETY: Will segfault if an invalid slot
    /// is set for
    pub unsafe fn set_slot_bool_unchecked(self, slot: Slot, value: bool) {
        wren_sys::wrenSetSlotBool(self.0.as_ptr(), slot, value);
    }

    /// SAFETY: Calling this on a slot that isn't a bool or a valid slot is undefined behavior
    pub unsafe fn get_slot_bool_unchecked(self, slot: Slot) -> bool {
        wren_sys::wrenGetSlotBool(self.0.as_ptr(), slot)
    }

    /// SAFETY: this is always non null but will segfault if an invalid slot
    /// is asked for
    /// And is not guarenteed to be a valid object
    pub unsafe fn get_slot_handle_unchecked(self, slot: Slot) -> Handle {
        NonNull::new_unchecked(wren_sys::wrenGetSlotHandle(self.0.as_ptr(), slot))
    }

    /// SAFETY: Calling this on a slot that isn't a bool or a valid slot is undefined behavior
    pub unsafe fn get_slot_double_unchecked(self, slot: Slot) -> f64 {
        wren_sys::wrenGetSlotDouble(self.0.as_ptr(), slot)
    }

    /// SAFETY: this is always non null but will segfault if an invalid slot
    /// is asked for
    /// MAYBE: Will seg fault if the variable does not exist?
    /// Still need to set up module resolution
    pub unsafe fn get_variable_unchecked<Module, Name>(self, module: Module, name: Name, slot: Slot)
    where
        Module: AsRef<str>,
        Name: AsRef<str>,
    {
        let vm = self.0;
        let module = CString::new(module.as_ref()).unwrap();
        let name = CString::new(name.as_ref()).unwrap();

        wrenGetVariable(vm.as_ptr(), module.as_ptr(), name.as_ptr(), slot);
    }

    pub fn make_call_handle<Signature>(self, signature: Signature) -> Handle
    where
        Signature: AsRef<str>,
    {
        let vm = self.0;
        let signature = CString::new(signature.as_ref()).unwrap();
        // SAFETY: this function is always safe to call but may be unsafe to use the handle it returns
        // as that handle might not be valid
        unsafe { NonNull::new_unchecked(wrenMakeCallHandle(vm.as_ptr(), signature.as_ptr())) }
    }

    /// Safety: Will segfault if used with an invalid method
    pub unsafe fn call(self, method: Handle) -> Result<(), InterpretResultErrorKind> {
        let vm = self.0;
        let result = wrenCall(vm.as_ptr(), method.as_ptr());

        InterpretResultErrorKind::new_from_result(result)
    }

    pub unsafe fn release_handle_unchecked(self, handle: Handle) {
        wrenReleaseHandle(self.0.as_ptr(), handle.as_ptr());
    }

    pub fn ensure_slots(self, num_slots: Slot) {
        // SAFETY: this one is always safe to call even if the value is negative
        unsafe {
            wren_sys::wrenEnsureSlots(self.0.as_ptr(), num_slots);
        }
    }
}

#[derive(Debug)]
pub struct ErrorContext<'s> {
    pub module: &'s str,
    pub line: i32,
    pub msg: &'s str,
}

#[derive(Debug)]
pub enum ErrorKind<'s> {
    Compile(ErrorContext<'s>),
    Runtime(&'s str),
    Stacktrace(ErrorContext<'s>),
    Unknown(WrenErrorType, ErrorContext<'s>),
}

#[derive(Debug)]
pub enum InterpretResultErrorKind {
    Compile,
    Runtime,
    Unknown(WrenInterpretResult),
}

impl InterpretResultErrorKind {
    const fn new_from_result(result: u32) -> Result<(), Self> {
        match result {
            wren_sys::WrenInterpretResult_WREN_RESULT_COMPILE_ERROR => Err(Self::Compile),
            wren_sys::WrenInterpretResult_WREN_RESULT_RUNTIME_ERROR => Err(Self::Runtime),
            wren_sys::WrenInterpretResult_WREN_RESULT_SUCCESS => Ok(()),
            kind => Err(Self::Unknown(kind)),
        }
    }
}

#[allow(unused_variables)]
// We define empty defaults here so that the user can define what they want
pub trait VmUserData {
    fn resolve_module(&mut self, resolver: &str, name: &str) -> Option<CString> {
        CString::new(name.to_string()).ok()
    }
    fn load_module(&mut self, name: &str) -> Option<CString> {
        None
    }
    fn bind_foreign_method(
        &mut self,
        module: &str,
        classname: &str,
        is_static: bool,
        signature: &str,
    ) -> Option<ForeignMethod> {
        unsafe { std::mem::zeroed() }
    }
    // Default behavior is to return a struct with fields nulled out
    // so this is fine
    fn bind_foreign_class(
        &mut self,
        module: &str,
        classname: &str,
    ) -> wren_sys::WrenForeignClassMethods {
        unsafe { std::mem::zeroed() }
    }
    fn on_write(&mut self, vm: VMPtr, text: &str) {}
    fn on_error(&mut self, vm: VMPtr, kind: ErrorKind) {}
}

pub struct Vm<V> {
    vm: VMPtr,
    // This value is held here so that it is
    // disposed of properly when execution is finished
    // but it isn't actually used in the struct
    _user_data: Pin<Box<RefCell<V>>>,
}

impl<V> Drop for Vm<V> {
    fn drop(&mut self) {
        unsafe { wrenFreeVM(self.vm.0.as_ptr()) }
    }
}

impl<V> Vm<V>
where
    V: VmUserData,
{
    pub fn new(user_data: V) -> Option<Self> {
        unsafe {
            let mut config: WrenConfiguration = MaybeUninit::zeroed().assume_init();
            wrenInitConfiguration(&mut config);

            // TODO: Check if this is a zst and don't allocate space if not
            let user_data = Box::pin(RefCell::new(user_data));

            config.writeFn = Some(write_fn::<V>);
            config.errorFn = Some(error_fn::<V>);
            config.loadModuleFn = Some(load_module::<V>);
            config.resolveModuleFn = Some(resolve_module::<V>);
            config.bindForeignMethodFn = Some(bind_foreign_method::<V>);
            config.userData = user_data.as_ptr().cast::<c_void>();

            let vm = VMPtr(NonNull::new(wrenNewVM(&mut config))?);

            Some(Self {
                vm,
                _user_data: user_data,
            })
        }
    }

    pub const fn get_ptr(&self) -> VMPtr {
        self.vm
    }

    pub fn interpret<M, S>(&self, module: M, source: S) -> Result<(), InterpretResultErrorKind>
    where
        M: AsRef<str>,
        S: AsRef<str>,
    {
        unsafe {
            let module = CString::new(module.as_ref()).unwrap();
            let source = CString::new(source.as_ref()).unwrap();
            let result = wrenInterpret(self.vm.0.as_ptr(), module.as_ptr(), source.as_ptr());

            InterpretResultErrorKind::new_from_result(result)
        }
    }
}