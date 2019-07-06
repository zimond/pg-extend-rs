// Copyright 2018 Benjamin Fry <benjaminfry@me.com>
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! A Postgres Allocator

use std::ffi::c_void;
use std::marker::{PhantomData, PhantomPinned};
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;

use crate::pg_sys;

/// An allocattor which uses the palloc and pfree functions available from Postgres.
///
/// This is managed by Postgres and guarantees that all memory is freed after a transaction completes.
pub struct PgAllocator(ManuallyDrop<Box<pg_sys::MemoryContextData>>);

impl PgAllocator {
    /// Instantiate a PgAllocator from the raw pointer.
    unsafe fn from_raw(context: *mut pg_sys::MemoryContextData) -> Self {
        Self(ManuallyDrop::new(Box::from_raw(context)))
    }

    /// Establishes a PgAllocator from the current default context.
    pub fn current_context() -> Self {
        unsafe { Self::from_raw(pg_sys::CurrentMemoryContext) }
    }

    // pub fn alloc<'mc, T>(&'mc self) -> PgAllocated<'mc, T>
    // where
    //     T: 'mc,
    // {
    //     let size = mem::size_of::<T>();
    //     // TODO: is there anything we need to do in terms of layout, etc?
    //     //let ptr = pg_sys::palloc(size) as *mut u8;
    //     unsafe {
    //         let ptr = crate::guard_pg(|| {
    //             pg_sys::MemoryContextAllocZeroAligned(
    //                 self.0.deref().deref() as *const _ as *mut _,
    //                 size,
    //             )
    //         });

    //         PgAllocated::from_raw(mem::transmute(ptr), self)
    //     }
    // }

    unsafe fn dealloc<T: ?Sized>(&self, pg_data: *mut T) {
        // TODO: see mctx.c in Postgres' source this probably needs more validation
        let ptr = pg_data as *mut c_void;
        //  pg_sys::pfree(pg_data as *mut c_void)
        let methods = *self.0.methods;
        crate::guard_pg(|| {
            methods.free_p.expect("free_p is none")(
                self.0.deref().deref() as *const _ as *mut _,
                ptr,
            );
        });
    }
}

/// Types that were allocated by Postgres
///
/// Any data allocated by Postgres or being returned to Postgres for management must be stored in this value.
pub struct PgAllocated<'mc, T: 'mc + RawPtr> {
    inner: Option<ManuallyDrop<T>>,
    allocator: &'mc PgAllocator,
    _disable_send_sync: PhantomData<NonNull<&'mc T>>,
    _not_unpin: PhantomPinned,
}

impl<'mc, T: RawPtr> PgAllocated<'mc, T>
where
    T: 'mc + RawPtr,
{
    /// Creates a new Allocated type from Postgres.
    ///
    /// This does not allocate, it associates the lifetime of the Allocator to this type.
    ///   it protects protects the wrapped type from being dropped by Rust, and uses the
    ///   associated Postgres Allocator for freeing the backing memory.
    pub unsafe fn from_raw(
        memory_context: &'mc PgAllocator,
        ptr: *mut <T as RawPtr>::Target,
    ) -> Self {
        PgAllocated {
            inner: Some(ManuallyDrop::new(T::from_raw(ptr))),
            allocator: memory_context,
            _disable_send_sync: PhantomData,
            _not_unpin: PhantomPinned,
        }
    }
}

impl<'mc, T: 'mc + RawPtr> Deref for PgAllocated<'mc, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner
            .as_ref()
            .expect("invalid None while PgAllocated is live")
            .deref()
    }
}

impl<'mc, T: 'mc + RawPtr> DerefMut for PgAllocated<'mc, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // TODO: instead of requiring Option here, swap the opinter with 0, and allow free on 0, which is safe.
        self.inner
            .as_mut()
            .expect("invalid None while PgAllocated is live")
            .deref_mut()
    }
}

impl<'mc, T: RawPtr> Drop for PgAllocated<'mc, T> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            unsafe {
                // TODO: do we need to run the drop on the inner type?
                // let ptr: *mut T = mem::transmute(inner.deref_mut().deref_mut());
                let ptr: *mut _ = ManuallyDrop::into_inner(inner).into_raw();
                self.allocator.dealloc(ptr);
            }
        }
    }
}

/// Types which implement this can be converted from pointers to their Rust type and vice versa.
pub trait RawPtr {
    type Target;

    /// Instantiate the type from the pointer
    unsafe fn from_raw(ptr: *mut Self::Target) -> Self;

    /// Consume this and return the pointer.
    unsafe fn into_raw(self) -> *mut Self::Target;
}

impl RawPtr for std::ffi::CString {
    type Target = std::os::raw::c_char;

    unsafe fn from_raw(ptr: *mut std::os::raw::c_char) -> Self {
        std::ffi::CString::from_raw(ptr)
    }

    unsafe fn into_raw(self) -> *mut std::os::raw::c_char {
        self.into_raw()
    }
}