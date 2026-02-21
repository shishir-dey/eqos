//! # Kernel
//!
//! Top-level kernel initialization and public API for EqOS.
//!
//! The kernel manages the global scheduler instance, provides task creation
//! and lifecycle APIs, and coordinates system startup. All public functions
//! use critical sections to ensure interrupt safety.
//!
//! ## Startup Sequence
//!
//! ```text
//! reset_handler (cortex-m-rt)
//!   └─► main()
//!         ├─► kernel::init()        ← Configure peripherals
//!         ├─► kernel::create_task() ← Register tasks (×N)
//!         └─► kernel::start()       ← Launch scheduler (no return)
//!               ├─► Configure SysTick
//!               ├─► Set interrupt priorities
//!               └─► Start first task via arch::start_first_task()
//! ```

use crate::arch::cortex_m4;
use crate::scheduler::Scheduler;
use crate::task::{TaskConfig, Strategy};
use crate::sync;

// ---------------------------------------------------------------------------
// Global scheduler instance
// ---------------------------------------------------------------------------

/// Global scheduler instance.
///
/// # Safety
/// Accessed via `SCHEDULER_PTR` which is set during `init()`.
/// All access is through critical sections or from ISR context
/// (where interrupts are already serialized by priority).
static mut SCHEDULER: Scheduler = Scheduler::new();

/// Raw pointer to the global scheduler. Used by the arch layer
/// (PendSV, SysTick handlers) which cannot easily use references.
///
/// # Safety
/// Set once during `init()`, read from ISR context.
#[no_mangle]
pub static mut SCHEDULER_PTR: *mut Scheduler = core::ptr::null_mut();

// ---------------------------------------------------------------------------
// Kernel API
// ---------------------------------------------------------------------------

/// Initialize the EqOS kernel.
///
/// Must be called before any other kernel function. Sets up the global
/// scheduler and its pointer for ISR access.
///
/// # Safety
/// Must be called exactly once, from the main thread, before starting
/// the scheduler.
pub fn init() {
    unsafe {
        SCHEDULER = Scheduler::new();
        SCHEDULER_PTR = &mut SCHEDULER as *mut Scheduler;
    }
}

/// Create a new task and register it with the scheduler.
///
/// # Parameters
/// - `entry`: Task entry function. Must be `extern "C" fn() -> !` (never returns).
/// - `config`: Static task configuration (priority, deadline, WCET, etc.).
/// - `strategy`: Initial game-theory strategy (Cooperative or Selfish).
///
/// # Returns
/// - `Ok(task_id)`: The task's index in the scheduler array.
/// - `Err(())`: The task array is full (`MAX_TASKS` reached).
///
/// # Example
/// ```ignore
/// let config = TaskConfig {
///     priority: 3,
///     deadline_ticks: 100,
///     wcet_ticks: 20,
///     affinity_mask: 0x01,
///     time_slice: 10,
/// };
/// kernel::create_task(my_task_fn, config, Strategy::Cooperative).unwrap();
/// ```
pub fn create_task(
    entry: extern "C" fn() -> !,
    config: TaskConfig,
    strategy: Strategy,
) -> Result<usize, ()> {
    sync::critical_section(|_cs| unsafe {
        (*SCHEDULER_PTR).create_task(entry, config, strategy)
    })
}

/// Start the EqOS scheduler. **Does not return.**
///
/// Configures the SysTick timer, sets interrupt priorities, and launches
/// the first task. After this call, the system is fully preemptive and
/// the game-theory scheduler is active.
///
/// # Safety
/// - `init()` must have been called.
/// - At least one task must have been created.
/// - Must be called from the main thread (not from an ISR).
///
/// # Panics
/// Loops forever if no tasks have been created (does not panic,
/// as panic infrastructure is minimal in no_std).
pub fn start(mut core_peripherals: cortex_m::Peripherals) -> ! {
    // Configure SysTick timer
    cortex_m4::configure_systick(&mut core_peripherals.SYST);

    // Set PendSV and SysTick to lowest priority
    cortex_m4::set_interrupt_priorities();

    // Get the first task's stack pointer and launch
    let first_sp = sync::critical_section(|_cs| unsafe {
        let scheduler = &mut *SCHEDULER_PTR;
        if scheduler.task_count == 0 {
            // No tasks — spin forever
            loop {
                cortex_m::asm::wfi();
            }
        }
        // Schedule the first task
        let first = scheduler.schedule();
        scheduler.tasks[first].stack_pointer as *const u32
    });

    unsafe {
        cortex_m4::start_first_task(first_sp);
    }
}

/// Voluntarily yield the CPU from the current task.
///
/// This is the primary cooperative mechanism. Calling this function:
/// 1. Records a voluntary yield in the task's payoff metrics
/// 2. Resets the task's time slice
/// 3. Triggers a reschedule via PendSV
///
/// Tasks that yield frequently receive cooperation bonuses, improving
/// their effective scheduling priority over time.
pub fn yield_task() {
    sync::critical_section(|_cs| unsafe {
        (*SCHEDULER_PTR).yield_current();
    });
    cortex_m4::trigger_pendsv();
}
