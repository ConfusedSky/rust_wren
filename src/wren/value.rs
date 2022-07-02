#![allow(unsafe_code)]
#![allow(clippy::module_name_repetitions)]

use std::{ffi::CString, ptr::NonNull};

use wren_sys as ffi;

use super::{
    context::{Foreign, Location, Native, Raw as RawContext},
    Handle, Slot,
};

pub type TryGetResult<'wren, T> = Result<T, Handle<'wren>>;

#[derive(PartialEq, Eq, Debug)]
pub enum WrenType {
    Bool,
    Num,
    Foreign,
    List,
    Map,
    Null,
    String,
    Unknown,
}

impl From<ffi::WrenType> for WrenType {
    fn from(other: ffi::WrenType) -> Self {
        match other {
            ffi::WrenType_WREN_TYPE_BOOL => Self::Bool,
            ffi::WrenType_WREN_TYPE_NUM => Self::Num,
            ffi::WrenType_WREN_TYPE_FOREIGN => Self::Foreign,
            ffi::WrenType_WREN_TYPE_LIST => Self::List,
            ffi::WrenType_WREN_TYPE_MAP => Self::Map,
            ffi::WrenType_WREN_TYPE_NULL => Self::Null,
            ffi::WrenType_WREN_TYPE_STRING => Self::String,
            _ => Self::Unknown,
        }
    }
}

struct SlotStorage<'wren> {
    vm: RawContext<'wren, Foreign>,
    slot: Slot,
    handle: Handle<'wren>,
}

impl<'wren> Drop for SlotStorage<'wren> {
    fn drop(&mut self) {
        // Slots are counted from 0 so to store this slot
        // we must ensure slot + 1 exist
        unsafe {
            self.vm.ensure_slots(self.slot + 1);
            self.handle.set_slot(&mut self.vm, self.slot);
        }
    }
}

unsafe fn store_slot<'wren, L: Location>(
    vm: &mut RawContext<'wren, L>,
    slot: Slot,
) -> SlotStorage<'wren> {
    let vm = vm.as_foreign_mut();
    // Same idea as above for the drop function
    vm.ensure_slots(slot + 1);
    let handle = Handle::get_slot(vm, slot);
    SlotStorage {
        vm: vm.clone(),
        slot,
        handle,
    }
}

pub trait SetValue<'wren, L: Location> {
    /// Number of additional slots that need to be allocated to use this
    const REQUIRED_SLOTS: Slot;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot);
}

pub trait GetValue<'wren, L: Location> {
    /// NOTE THE IMPLEMENTATION OF GET FROM VM SHOULD BE SAFE REGARDLESS
    /// OF WHAT IS IN THE SLOT
    /// `get_from_vm` is unsafe because it's not guarenteed that
    /// the slot is a valid slot
    unsafe fn get_slot(vm: &mut RawContext<'wren, L>, slot: Slot) -> Self;
}

// () is implemented to allow skipping slots
// and to set send null to the vm
impl<'wren, L: Location> SetValue<'wren, L> for () {
    const REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        ffi::wrenSetSlotNull(vm.as_ptr(), slot);
    }
}

impl<'wren, L: Location> GetValue<'wren, L> for () {
    unsafe fn get_slot(_vm: &mut RawContext<'wren, L>, _slot: Slot) -> Self {}
}

impl<'wren, L: Location, T: SetValue<'wren, L>> SetValue<'wren, L> for &T {
    const REQUIRED_SLOTS: Slot = T::REQUIRED_SLOTS;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        (*self).set_slot(vm, slot);
    }
}

impl<'wren, L: Location> SetValue<'wren, L> for Handle<'wren> {
    const REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        ffi::wrenSetSlotHandle(vm.as_ptr(), slot, self.as_ptr());
    }
}
impl<'wren, L: Location> GetValue<'wren, L> for Handle<'wren> {
    // We are always able to Get<'wren> a handle from wren
    unsafe fn get_slot(vm: &mut RawContext<'wren, L>, slot: Slot) -> Self {
        let handle = ffi::wrenGetSlotHandle(vm.as_ptr(), slot);
        Self::new_unchecked(vm.as_foreign(), NonNull::new_unchecked(handle))
    }
}

unsafe fn send_iterator_to_vm<'wren, L: Location, T: SetValue<'wren, L>, I: Iterator<Item = T>>(
    iterator: I,
    vm: &mut RawContext<'wren, L>,
    slot: Slot,
) {
    let mut list = None;
    ffi::wrenSetSlotNewList(vm.as_ptr(), slot);
    // Store the next slot so we don't overwrite it's value
    // Or use the previous slot instead of juggling slots
    let (_store, item_slot) = if slot == 0 {
        //  We should store the list in a handle as well just
        // in case send_to_vm overwrites [slot]
        list = Some(Handle::get_slot(vm, slot));
        (Some(store_slot(vm, slot + 1)), slot + 1)
    } else {
        (None, slot - 1)
    };

    for value in iterator {
        value.set_slot(vm, item_slot);
        // If we had to store the list earlier
        // then it needs to be sent to the vm again
        if let Some(list) = &list {
            list.set_slot(vm, slot);
        }
        ffi::wrenInsertInList(vm.as_ptr(), slot, -1, item_slot);
    }
}

impl<'wren, L: Location, T: SetValue<'wren, L>> SetValue<'wren, L> for Vec<T> {
    // This needs at least one for moving values into the wren list as well as
    // any additional slots for T's initialization
    const REQUIRED_SLOTS: Slot = 1 + T::REQUIRED_SLOTS;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        send_iterator_to_vm(self.iter(), vm, slot);
    }
}

impl<'wren, L: Location, T: GetValue<'wren, L>> GetValue<'wren, L> for Vec<T> {
    unsafe fn get_slot(vm: &mut RawContext<'wren, L>, slot: Slot) -> Self {
        // Store the next slot so we don't overwrite it's value
        // Or use the previous slot instead of juggling slots
        let (_store, item_slot) = if slot == 0 {
            // Make sure the slot that we are storing actually exits
            (Some(store_slot(vm, slot + 1)), slot + 1)
        } else {
            (None, slot - 1)
        };

        let mut vec = vec![];

        // TODO: Handle this better than just returning an empty vec
        let ty = vm.get_slot_type(slot);
        // if cfg!(debug_assertions) {
        // assert!(ty == WrenType::List, "Unable to get non list value as Vec");
        // }

        if ty == WrenType::List {
            let count = ffi::wrenGetListCount(vm.as_ptr(), slot);

            for i in 0..count {
                ffi::wrenGetListElement(vm.as_ptr(), slot, i, item_slot);
                // Wren arrays can be mixed items
                // So this will default a couple of the items presumably
                vec.push(T::get_slot(vm, item_slot));
            }
        }

        vec
    }
}

impl<'wren, L: Location, T: SetValue<'wren, L>> SetValue<'wren, L> for [T] {
    // This needs at least one for moving values into the wren list as well as
    // any additional slots for T's initialization
    const REQUIRED_SLOTS: Slot = 1 + T::REQUIRED_SLOTS;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        send_iterator_to_vm(self.iter(), vm, slot);
    }
}

impl<'wren, L: Location> SetValue<'wren, L> for CString {
    const REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        ffi::wrenSetSlotString(vm.as_ptr(), slot, self.as_ptr());
    }
}

unsafe fn send_string_to_vm<S: AsRef<str>, L: Location>(
    vm: &mut RawContext<L>,
    value: S,
    slot: Slot,
) {
    let str = value.as_ref();
    wren_sys::wrenSetSlotBytes(
        vm.as_ptr(),
        slot,
        str.as_ptr().cast(),
        // The len should always be valid
        str.len().try_into().expect("Invalid length for str"),
    );
}

impl<'wren, L: Location> SetValue<'wren, L> for &str {
    const REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        send_string_to_vm(vm, self, slot);
    }
}

unsafe fn generic_get_string<L: Location>(vm: &mut RawContext<L>, slot: Slot) -> Option<String> {
    let ty = vm.get_slot_type(slot);
    match ty {
        WrenType::Bool => Some(ffi::wrenGetSlotBool(vm.as_ptr(), slot).to_string()),
        WrenType::Null => Some("null".to_string()),
        WrenType::String => {
            let mut len = 0;
            let ptr = ffi::wrenGetSlotBytes(vm.as_ptr(), slot, &mut len).cast();
            let len = len.try_into().unwrap();
            let slice = std::slice::from_raw_parts(ptr, len);
            Some(String::from_utf8_lossy(slice).to_string())
        }
        _ => None,
    }
}

impl<'wren, L: Location> SetValue<'wren, L> for String {
    const REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        send_string_to_vm(vm, self, slot);
    }
}

impl<'wren> GetValue<'wren, Native> for String {
    unsafe fn get_slot(vm: &mut RawContext<'wren, Native>, slot: Slot) -> Self {
        generic_get_string(vm, slot).unwrap_or_else(|| {
            // Fall back to calling to string on the returned object
            // ONLY WORKS IN NATIVE CODE
            let system_methods = vm.get_system_methods();
            let handle = Handle::get_slot(vm, slot);
            let to_string = &system_methods.object_to_string;

            // NOTE: this might break the native stack so it might need to be
            // saved off first
            // Note this shouldn't recurse because the second call
            // will always be called on a "real" string
            vm.call(&handle, to_string, &())
                .expect("toString should never fail on a valid wren handle")
        })
    }
}

impl<'wren> GetValue<'wren, Foreign> for TryGetResult<'wren, String> {
    unsafe fn get_slot(vm: &mut RawContext<'wren, Foreign>, slot: Slot) -> Self {
        generic_get_string(vm, slot).ok_or_else(|| Handle::get_slot(vm, slot))
    }
}

impl<'wren, L: Location> SetValue<'wren, L> for f64 {
    const REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        ffi::wrenSetSlotDouble(vm.as_ptr(), slot, *self);
    }
}

impl<'wren, L: Location> GetValue<'wren, L> for f64 {
    unsafe fn get_slot(vm: &mut RawContext<'wren, L>, slot: Slot) -> Self {
        if WrenType::Num == vm.get_slot_type(slot) {
            ffi::wrenGetSlotDouble(vm.as_ptr(), slot)
        } else {
            Self::NAN
        }
    }
}

impl<'wren, L: Location> SetValue<'wren, L> for bool {
    const REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slot(&self, vm: &mut RawContext<'wren, L>, slot: Slot) {
        ffi::wrenSetSlotBool(vm.as_ptr(), slot, *self);
    }
}

impl<'wren, L: Location> GetValue<'wren, L> for bool {
    unsafe fn get_slot(vm: &mut RawContext<'wren, L>, slot: Slot) -> Self {
        let ty = vm.get_slot_type(slot);
        match ty {
            WrenType::Bool => ffi::wrenGetSlotBool(vm.as_ptr(), slot),
            WrenType::Null => false,
            _ => true,
        }
    }
}

pub trait SetArgs<'wren, L: Location> {
    const COUNT: usize;
    const TOTAL_REQUIRED_SLOTS: Slot;
    unsafe fn set_slots(&self, vm: &mut RawContext<'wren, L>, offset: u16);
    /// This fn should probably never be used directly since it only existed
    /// before required slots was a constant
    unsafe fn set_wren_stack_unchecked(
        &self,
        vm: &mut RawContext<'wren, L>,
        num_slots: Slot,
        offset: u16,
    ) {
        vm.ensure_slots(num_slots + Slot::from(offset));
        self.set_slots(vm, offset);
    }
    /// Sets the values on the [vm]'s stack
    /// also accepts a [offset] parameter that allows the items to be shifted up
    /// to allow other items to be passed beforehand
    fn set_wren_stack(&self, vm: &mut RawContext<'wren, L>, offset: u16) {
        // This is guarenteed to be safe because we ensured that we had enough
        // slots for T using TOTAL_REQUIRED_SLOTS
        unsafe {
            self.set_wren_stack_unchecked(vm, Self::TOTAL_REQUIRED_SLOTS, offset);
        }
    }
}

impl<'wren, L: Location> SetArgs<'wren, L> for () {
    const COUNT: usize = 0;
    const TOTAL_REQUIRED_SLOTS: Slot = 1;
    unsafe fn set_slots(&self, _: &mut RawContext<'wren, L>, _: u16) {
        // An empty arg list should do nothing
        // This currently sends a null
        // ().send_to_vm(vm, Slot::from(offset));
    }
}

impl<'wren, L: Location, T: SetValue<'wren, L> + ?Sized> SetArgs<'wren, L> for &T {
    const COUNT: usize = 1;
    const TOTAL_REQUIRED_SLOTS: Slot = T::REQUIRED_SLOTS;
    unsafe fn set_slots(&self, vm: &mut RawContext<'wren, L>, offset: u16) {
        self.set_slot(vm, Slot::from(offset));
    }
}

const fn _const_max_helper(a: Slot, b: Slot) -> Slot {
    [a, b][(a < b) as usize]
}

macro_rules! expand_TOTAL_REQUIRED_SLOTS {
    (@step $i:expr, $x:ty) => (<$x>::REQUIRED_SLOTS+ $i);
    (@step $i:expr, $x:ty, $($y:ty),+ $(,)?) => (
        _const_max_helper(
            <$x>::REQUIRED_SLOTS+ $i,
            expand_TOTAL_REQUIRED_SLOTS!(@step $i + 1, $($y),+),
        )
    );
    ($x:ty, $($y:ty),+ $(,)?) => (
        expand_TOTAL_REQUIRED_SLOTS!(@step 0i32, $x, $($y),+)
    )
}

macro_rules! expand_set_slots {
    ($self:ident, $vm:ident, $method:ident, $offset:expr, $i:tt) => {
        $self.$i.$method($vm, $i + $offset as Slot);
    };
    ($self:ident, $vm:ident, $method:ident, $offset:expr, $i:tt, $($xs:tt),+ $(,)?) => {
        expand_set_slots!($self, $vm, $method, $offset, $( $xs ), *);
        expand_set_slots!($self, $vm, $method, $offset, $i);
    };
}

macro_rules! count {
    () => (0usize);
    ( $x:tt $($xs:tt)* ) => (1usize + count!($($xs)*));
}

macro_rules! impl_set_args_meta {
    ($location:ty, $( $xs:ident = $i:tt ), *) => {
        impl<'wren, $( $xs: SetValue<'wren, $location>, )*> SetArgs<'wren, $location> for ($( &$xs, )*) {
            const COUNT: usize = count!( $( $xs ) * );
            const TOTAL_REQUIRED_SLOTS: Slot =
                expand_TOTAL_REQUIRED_SLOTS!($( $xs ), *);


            unsafe fn set_slots(&self, vm: &mut RawContext<'wren, $location>, offset: u16) {
                // Expansion happens in reverse order so previous slots
                // can be used when scratch slots are needed
                expand_set_slots!(self, vm, set_slot, offset, $( $i ), *);
            }
        }
    };
}

macro_rules! impl_set_args {
    ($( $xs:ident = $i:tt ), *) => {
        impl_set_args_meta!(Native, $( $xs = $i ), *);
        impl_set_args_meta!(Foreign, $( $xs = $i ), *);
    };
}

impl_set_args!(T0 = 0, T1 = 1);
impl_set_args!(T0 = 0, T1 = 1, T2 = 2);
impl_set_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3);
impl_set_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3, T4 = 4);
impl_set_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3, T4 = 4, T5 = 5);
impl_set_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3, T4 = 4, T5 = 5, T6 = 6);

pub trait GetArgs<'wren, L: Location> {
    unsafe fn get_slots(vm: &mut RawContext<'wren, L>) -> Self;
}

impl<'wren, L: Location, T: GetValue<'wren, L>> GetArgs<'wren, L> for T {
    unsafe fn get_slots(vm: &mut RawContext<'wren, L>) -> Self {
        T::get_slot(vm, 0)
    }
}

macro_rules! impl_get_args_meta {
    ($location:ty, $( $xs:ident = $i:tt ), *) => {
        impl<'wren, $( $xs: GetValue<'wren, $location>, )*> GetArgs<'wren, $location> for ($( $xs, )*) {
            unsafe fn get_slots(vm: &mut RawContext<'wren, $location>) -> Self{
                (
                    // Expansion happens in forward order so
                    // Previous slots can be reused for reads
                    $( $xs::get_slot(vm, $i) ), *
                )
            }
        }
    };
}

macro_rules! impl_get_args {
    ($( $xs:ident = $i:tt ), *) => {
        impl_get_args_meta!(Native, $( $xs = $i ), *);
        impl_get_args_meta!(Foreign, $( $xs = $i ), *);
    };
}

impl_get_args!(T0 = 0, T1 = 1);
impl_get_args!(T0 = 0, T1 = 1, T2 = 2);
impl_get_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3);
impl_get_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3, T4 = 4);
impl_get_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3, T4 = 4, T5 = 5);
impl_get_args!(T0 = 0, T1 = 1, T2 = 2, T3 = 3, T4 = 4, T5 = 5, T6 = 6);

#[cfg(test)]
mod test {
    use crate::{
        call_test_case,
        wren::{
            context::{Foreign, Native},
            test::create_test_vm,
        },
    };

    use super::SetArgs;

    #[test]
    fn test_slot_size() {
        macro_rules! meta_test {
            ($location:ty) => {{
                type L = $location;
                assert_eq!(<&f64 as SetArgs<L>>::TOTAL_REQUIRED_SLOTS, 1);
                assert_eq!(<(&f64, &f64) as SetArgs<L>>::TOTAL_REQUIRED_SLOTS, 2);
                assert_eq!(
                    <(&Vec<Vec<f64>>, &f64) as SetArgs<L>>::TOTAL_REQUIRED_SLOTS,
                    3
                );
                assert_eq!(
                    <(&f64, &Vec<Vec<f64>>) as SetArgs<L>>::TOTAL_REQUIRED_SLOTS,
                    4
                );
                assert_eq!(<(&f64, &f64, &f64) as SetArgs<L>>::TOTAL_REQUIRED_SLOTS, 3);
                assert_eq!(
                    <(&f64, &f64, &f64, &f64) as SetArgs<L>>::TOTAL_REQUIRED_SLOTS,
                    4
                );
            }};
        }
        meta_test!(Foreign);
        meta_test!(Native);
    }

    // Test that all values other than null and false are falsy
    #[test]
    #[allow(non_snake_case)]
    fn test_bool() {
        let source = "class Test {
                static returnTrue { true }
                static returnTrue() { true }
                static returnFalse() { false }
                static returnNull() { null }
                static returnValue(value) { value }
                static returnNegate(value) { !value }
            }";

        let (mut vm, Test) = create_test_vm(source, |_| {});
        let context = vm.get_context();

        // False cases
        call_test_case!(bool, context { Test.returnNull() } == false);
        call_test_case!(bool, context { Test.returnFalse() } == false);
        call_test_case!(bool, context { Test.returnValue(false) } == false);
        call_test_case!(bool, context { Test.returnNegate(true) } == false);
        call_test_case!(bool, context { Test.returnNegate("") } == false);

        // True cases
        call_test_case!(bool, context { Test.returnTrue } == true);
        call_test_case!(bool, context { Test.returnTrue() } == true);
        call_test_case!(bool, context { Test.returnValue(true) } == true);
        call_test_case!(bool, context { Test.returnNegate(false) } == true);
        call_test_case!(bool, context { Test.returnValue("") } == true);
        call_test_case!(bool, context { Test.returnValue("Test") } == true);
        call_test_case!(bool, context { Test.returnValue(Test) } == true);
        call_test_case!(bool, context { Test.returnValue(vec![1.0]) } == true);
        call_test_case!(bool, context { Test.returnValue(1.0) } == true);
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_string() {
        let source = "class Test {
                static returnTrue { true }
                static returnTrue() { true }
                static returnFalse() { false }
                static returnNull() { null }
                static returnValue(value) { value }
                static sendMulti(value, value2) { value.toString + value2.toString }
                static returnMap {
                    var m = {}
                    m[\"test\"] = 1
                    m[15] = Test
                    return m
                }
            }";

        let (mut vm, Test) = create_test_vm(source, |_| {});
        let context = vm.get_context();

        call_test_case!(String, context { Test.returnNull() } == "null");
        call_test_case!(String, context { Test.returnFalse() } == "false");
        call_test_case!(String, context { Test.returnValue(false) } == "false");
        call_test_case!(String, context { Test.returnTrue } == "true");
        call_test_case!(String, context { Test.returnTrue() } == "true");
        call_test_case!(String, context { Test.returnValue("") } == "");
        call_test_case!(String, context { Test.returnValue("Test") } == "Test");
        call_test_case!(String, context { Test.returnValue("Test".to_string()) } == "Test");
        call_test_case!(String, context { Test.returnValue(Test) } == "Test");
        call_test_case!(String, context { Test.returnValue(vec![1.0]) } == "[1]");
        call_test_case!(String, context { Test.returnValue(vec!["1.0", "Other"]) } == "[1.0, Other]");
        call_test_case!(String, context { Test.returnValue(1.0) } == "1");
        call_test_case!(String, context { Test.returnMap } == "{test: 1, 15: Test}");
        call_test_case!(String, context { Test.sendMulti("Test", vec![1.0]) } == "Test[1]");
        call_test_case!(String, context { Test.sendMulti(vec!["One Two"], "Test") } == "[One Two]Test");
    }

    #[test]
    #[allow(non_snake_case)]
    fn test_vec() {
        let source = "class Test {
                static returnTrue { true }
                static returnTrue() { true }
                static returnFalse() { false }
                static returnNull() { null }
                static returnValue(value) { value }
                static nestedArray { [[[1]]] }
                static sendMulti(value, value2) { value.toString + value2.toString }
                static returnMap {
                    var m = {}
                    m[\"test\"] = 1
                    m[15] = Test
                    return m
                }
            }";

        let (mut vm, Test) = create_test_vm(source, |_| {});
        let context = vm.get_context();

        call_test_case!(Vec<String>, context { 
            Test.returnNull() } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnFalse() } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnValue(false) } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnTrue } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnTrue() } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnValue("") } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnValue("Test") } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnValue("Test".to_string()) } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnValue(Test) } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnValue(vec![1.0]) } == vec!["1"]);
        call_test_case!(Vec<String>, context {
            Test.returnValue(vec!["1.0", "Other"]) } == vec!["1.0", "Other"]);
        call_test_case!(Vec<String>, context {
            Test.returnValue(1.0) } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.returnMap } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.sendMulti("Test", vec![1.0]) } == Vec::<String>::new());
        call_test_case!(Vec<String>, context {
            Test.sendMulti(vec!["One Two"], "Test") } == Vec::<String>::new());

        call_test_case!(Vec<Vec<String>>, context {
            Test.returnValue(vec![vec!["1.0", "Other"]]) } == vec![vec!["1.0", "Other"]]);

        call_test_case!(Vec<String>, context {
            Test.returnValue(
                vec![vec![vec!["1.0", "Other"]]]
            )
        } == vec!["[[1.0, Other]]"]);
        call_test_case!(Vec<Vec<Vec<f64>>>, context {
            Test.nestedArray
        } == vec![vec![vec![1.0_f64]]]);
    }
}
