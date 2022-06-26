use std::{marker::PhantomData, ptr::NonNull};

use wren_sys::{wrenReleaseHandle, WrenHandle};

use super::RawVMContext;

pub struct Handle<'wren> {
    vm: RawVMContext<'wren>,
    pointer: NonNull<WrenHandle>,
    phantom: PhantomData<WrenHandle>,
}

impl<'wren> Handle<'wren> {
    pub(crate) fn new(vm: &RawVMContext<'wren>, pointer: NonNull<WrenHandle>) -> Self {
        Self {
            vm: vm.clone(),
            pointer,
            phantom: PhantomData,
        }
    }

    pub(crate) const fn as_ptr(&self) -> *mut WrenHandle {
        self.pointer.as_ptr()
    }
}

impl<'wren> Drop for Handle<'wren> {
    fn drop(&mut self) {
        unsafe { wrenReleaseHandle(self.vm.as_ptr(), self.pointer.as_ptr()) }
    }
}
