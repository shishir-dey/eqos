//! # Synchronization Primitives
//!
//! Interrupt-safe critical section abstractions for the Cortex-M4.
//! All shared scheduler state must be accessed within a critical section
//! to prevent data races between the main thread and interrupt handlers.

use cortex_m::interrupt;

/// Execute a closure within a critical section (interrupts disabled).
///
/// This is the primary mechanism for safely accessing shared mutable state
/// in the EqOS kernel. Interrupts are disabled on entry and restored on exit,
/// ensuring atomicity of the enclosed operation.
///
/// # Usage
/// ```ignore
/// sync::critical_section(|_cs| {
///     // Access shared state safely
/// });
/// ```
///
/// # Performance
/// Keep critical sections as short as possible to minimize interrupt latency.
/// The Cortex-M4's interrupt tail-chaining makes short critical sections
/// relatively inexpensive.
#[inline]
pub fn critical_section<F, R>(f: F) -> R
where
    F: FnOnce(&interrupt::CriticalSection) -> R,
{
    interrupt::free(f)
}
