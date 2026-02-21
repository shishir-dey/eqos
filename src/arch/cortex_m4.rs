//! # Cortex-M4 Port Layer
//!
//! Hardware-specific code for the ARM Cortex-M4 (Thumb-2) processor.
//! Implements context switching via PendSV, SysTick timer configuration,
//! and interrupt management.
//!
//! ## Context Switch Mechanism
//!
//! The Cortex-M4 uses a split-stack model:
//! - **MSP** (Main Stack Pointer): Used by the kernel and interrupt handlers
//! - **PSP** (Process Stack Pointer): Used by tasks in Thread mode
//!
//! On exception entry, the hardware automatically stacks R0–R3, R12, LR, PC,
//! and xPSR onto the process stack. The PendSV handler manually saves and
//! restores R4–R11, which completes the full context save/restore.
//!
//! ## Interrupt Priorities
//!
//! - SysTick: Priority 0xFF (lowest) — can be preempted
//! - PendSV: Priority 0xFF (lowest) — runs only when no other ISR is active
//!
//! Both are set to the lowest priority to ensure that PendSV doesn't
//! preempt other interrupt handlers, maintaining real-time guarantees.

use cortex_m::peripheral::syst::SystClkSource;
use cortex_m::register;
use core::arch::asm;

use crate::config::{SYSTEM_CLOCK_HZ, TICK_HZ};

// ---------------------------------------------------------------------------
// SysTick configuration
// ---------------------------------------------------------------------------

/// Configure the SysTick timer for the scheduler tick.
///
/// Sets up SysTick to fire at `TICK_HZ` frequency using the processor
/// clock. Each tick triggers `SysTick_Handler` which calls `Scheduler::tick()`.
///
/// # Parameters
/// - `syst`: Mutable reference to the SysTick peripheral
pub fn configure_systick(syst: &mut cortex_m::peripheral::SYST) {
    let reload = SYSTEM_CLOCK_HZ / TICK_HZ - 1;
    syst.set_reload(reload);
    syst.clear_current();
    syst.set_clock_source(SystClkSource::Core);
    syst.enable_counter();
    syst.enable_interrupt();
}

// ---------------------------------------------------------------------------
// PendSV trigger
// ---------------------------------------------------------------------------

/// Trigger a PendSV exception to perform a context switch.
///
/// PendSV is the standard Cortex-M mechanism for deferred context switching.
/// It fires at the lowest priority, ensuring it only runs when no other
/// ISR is active. The SysTick handler calls this when rescheduling is needed.
///
/// Sets the PENDSVSET bit in the Interrupt Control and State Register (ICSR).
#[inline]
pub fn trigger_pendsv() {
    // ICSR address: 0xE000_ED04, PENDSVSET = bit 28
    const ICSR: *mut u32 = 0xE000_ED04 as *mut u32;
    unsafe {
        core::ptr::write_volatile(ICSR, 1 << 28);
    }
}

// ---------------------------------------------------------------------------
// Interrupt priority configuration
// ---------------------------------------------------------------------------

/// Set PendSV and SysTick to the lowest interrupt priority.
///
/// This ensures context switches (PendSV) never preempt application-level
/// ISRs, and SysTick doesn't interfere with higher-priority interrupts.
/// Both use priority 0xFF (lowest on Cortex-M4 with 4 priority bits = 0xF0).
pub fn set_interrupt_priorities() {
    unsafe {
        // System Handler Priority Register 3 (SHPR3): 0xE000_ED20
        // Bits [23:16] = PendSV priority
        // Bits [31:24] = SysTick priority
        let shpr3: *mut u32 = 0xE000_ED20 as *mut u32;
        let val = core::ptr::read_volatile(shpr3);
        let val = val | (0xFF << 16) | (0xFF << 24);
        core::ptr::write_volatile(shpr3, val);
    }
}

// ---------------------------------------------------------------------------
// First task launch
// ---------------------------------------------------------------------------

/// Start the first task by switching to PSP and branching to Thread mode.
///
/// This is called once during `kernel::start()` and never returns.
/// It sets up the processor to use PSP for Thread mode and jumps
/// to the first task's entry point via a fake exception return.
///
/// # Safety
/// Must only be called once, with a valid stack pointer.
pub unsafe fn start_first_task(psp: *const u32) {
    asm!(
        // Set PSP to the task's stack pointer (skip SW-saved R4-R11)
        "adds r0, #32",        // Skip 8 SW registers (8×4 = 32 bytes)
        "msr psp, r0",         // Set process stack pointer

        // Switch to PSP for Thread mode (set CONTROL.SPSEL = 1)
        "movs r0, #2",
        "msr control, r0",
        "isb",

        // Pop the hardware frame manually since we're not really returning from an exception
        "pop {{r0-r3, r12}}",  // R0-R3, R12
        "pop {{r4}}",          // LR (we discard task_exit here, task is noreturn)
        "pop {{r5}}",          // PC (task entry point)
        "pop {{r6}}",          // xPSR (discard, will be set by processor)

        // Branch to the task
        "cpsie i",             // Enable interrupts
        "bx r5",               // Jump to task entry

        in("r0") psp,
        options(noreturn)
    );
}

// ---------------------------------------------------------------------------
// PendSV handler (context switch)
// ---------------------------------------------------------------------------

/// PendSV exception handler — performs the actual context switch.
///
/// ## Sequence
/// 1. Save R4–R11 onto the current task's stack (PSP)
/// 2. Store the updated PSP into the current task's TCB
/// 3. Call the scheduler to select the next task
/// 4. Load the next task's PSP from its TCB
/// 5. Restore R4–R11 from the new task's stack
/// 6. Return from exception (hardware restores R0–R3, R12, LR, PC, xPSR)
///
/// # Safety
/// This is a naked function called directly by the NVIC. It must follow
/// the exact Cortex-M4 exception entry/exit convention.
#[no_mangle]
#[naked]
pub unsafe extern "C" fn PendSV() {
    asm!(
        // --- Save current context ---
        "mrs r0, psp",             // Get current PSP
        "stmdb r0!, {{r4-r11}}",   // Push R4-R11 onto task stack (decrement before store)

        // Store updated PSP into current TCB
        // r0 now points to saved context on the stack
        "bl {save_context}",       // save_context(r0: *mut u32)

        // --- Select next task ---
        "bl {do_schedule}",        // Returns new PSP in r0

        // --- Restore new context ---
        "ldmia r0!, {{r4-r11}}",   // Pop R4-R11 from new task stack
        "msr psp, r0",             // Set PSP to new task's stack

        // Return from exception using PSP (EXC_RETURN = 0xFFFFFFFD)
        "ldr r0, =0xFFFFFFFD",
        "bx r0",

        save_context = sym save_current_context,
        do_schedule = sym do_context_switch,
        options(noreturn)
    );
}

/// Save the current task's stack pointer. Called from PendSV.
///
/// # Safety
/// Called from assembly context with interrupts disabled.
#[no_mangle]
unsafe extern "C" fn save_current_context(psp: *mut u32) {
    let scheduler = &mut *crate::kernel::SCHEDULER_PTR;
    let current = scheduler.current_task;
    if current < scheduler.task_count {
        scheduler.tasks[current].stack_pointer = psp;
    }
}

/// Perform the scheduling decision and return the new task's PSP.
/// Called from PendSV.
///
/// # Safety
/// Called from assembly context.
#[no_mangle]
unsafe extern "C" fn do_context_switch() -> *mut u32 {
    let scheduler = &mut *crate::kernel::SCHEDULER_PTR;
    let next = scheduler.schedule();
    scheduler.tasks[next].stack_pointer
}

// ---------------------------------------------------------------------------
// SysTick handler
// ---------------------------------------------------------------------------

/// SysTick exception handler — scheduler tick entry point.
///
/// Called at `TICK_HZ` frequency. Updates scheduler state and triggers
/// PendSV if a context switch is needed.
#[no_mangle]
pub unsafe extern "C" fn SysTick() {
    let scheduler = &mut *crate::kernel::SCHEDULER_PTR;
    scheduler.tick();

    if scheduler.needs_reschedule {
        trigger_pendsv();
    }
}
