//! Tools for erasing the lifetime of a value by converting it to raw bytes,
//! and reconstructing the value from those bytes.
//!
//! This module provides two core operations:
//!
//! - [`obfuscate_lifetime`] — Await a future, obtain its output `T`, and
//!   convert it into an owned `Box<[u8]>` containing the raw bytes of `T`.
//!   This *erases* any lifetimes carried by `T` because bytes do not track
//!   lifetimes.
//!
//! - [`from_bytes_unchecked`] — Reconstruct a value of type `T` from a byte
//!   slice.  
//!   **This function is `unsafe`**, because not all byte patterns represent
//!   valid instances of all types.
//!
//! These functions are low-level building blocks for scoped-lifetime executor
//! machinery, task splitting, and other advanced systems where the `Output`
//! of a future must be stored without carrying any lifetime information.

use std::{
    future::Future,
    mem::{self, MaybeUninit},
    ptr,
    slice,
};

/// Converts the output of a future into an owned `Box<[u8]>` containing the
/// raw memory representation of the produced value.
///
/// This function’s purpose is to *erase the lifetime and type information*
/// of the value produced by `future.await`.  
/// Because the returned data is just bytes, it carries **no lifetime**.
///
/// # Panics
/// Never panics.
///
/// # Safety
/// This function is fully safe—*copying bytes out of a valid `T` is always safe*
/// — but reconstructing `T` using [`from_bytes_unchecked`] is not.
///
/// # Implementation Notes
///
/// - The function allocates a `Box<[u8]>` of size `size_of::<T>()`.
/// - It performs a byte-for-byte copy from the stack-allocated value.
/// - `mem::forget` prevents the original value from being dropped after bytes
///   have been copied, because its memory has effectively been moved out.
pub(crate) async fn obfuscate_lifetime<T>(future: impl Future<Output = T>) -> Box<[u8]> {
    let value = future.await;

    // Allocate a heap buffer with the correct size.
    let mut boxed = vec![0u8; mem::size_of::<T>()].into_boxed_slice();

    // Copy raw bytes from the result value.
    unsafe {
        ptr::copy_nonoverlapping(
            (&value as *const T).cast::<u8>(),
            boxed.as_mut_ptr(),
            mem::size_of::<T>(),
        );
    }

    // Prevent running Drop for value, since we've copied its bytes out.
    mem::forget(value);

    boxed
}

/// Reconstructs a value of type `T` from a byte slice containing its exact
/// memory representation.
///
/// This is the inverse of [`obfuscate_lifetime`].
///
/// # Safety
///
/// This function is **inherently unsafe**. The caller must guarantee:
///
/// - `bytes.len() == mem::size_of::<T>()`
/// - `bytes` contains a valid bit-pattern for type `T`  
///   (e.g. no invalid enum discriminant, no uninitialized padding, etc.)
/// - `T` must not require a more restrictive alignment than stack allocation
///   provides (always true for types that can exist normally in function local
///   scope)
///
/// Violating any of these conditions results in **undefined behavior**.s
pub(crate) unsafe fn from_bytes_unchecked<T>(bytes: &[u8]) -> T {
    assert_eq!(bytes.len(), mem::size_of::<T>());

    let mut maybe_uninit = MaybeUninit::<T>::uninit();

    // Obtain a byte slice pointing into the uninitialized T.
    let dst = slice::from_raw_parts_mut(
        maybe_uninit.as_mut_ptr().cast::<u8>(),
        mem::size_of::<T>(),
    );

    // Copy all bytes into the uninitialized value.
    dst.copy_from_slice(bytes);

    // Assume the produced value is valid.
    maybe_uninit.assume_init()
}
