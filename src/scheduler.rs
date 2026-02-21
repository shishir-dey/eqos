//! # Scheduler
//!
//! Core scheduling logic for EqOS. Implements a preemptive, priority-aware
//! scheduler that uses game-theory payoff adjustments to balance fairness,
//! throughput, and deadline compliance.
//!
//! ## Scheduling Algorithm
//!
//! At each SysTick interrupt:
//! 1. **Update metrics**: Increment tick counters, track CPU usage
//! 2. **Decrement time slice**: If expired, mark task as Ready
//! 3. **Periodic evaluation** (every `EVAL_FREQUENCY` ticks):
//!    a. Recompute payoff for each task via `game::compute_payoff()`
//!    b. Check equilibrium; update strategies if not stable
//!    c. Apply starvation prevention boosts
//! 4. **Select next task**: Highest effective-priority runnable task
//! 5. **Context switch**: If selected task differs from current, trigger PendSV
//!
//! ## Starvation Prevention
//!
//! Any task that receives zero CPU for `STARVATION_THRESHOLD` ticks gets a
//! temporary priority boost, ensuring eventual execution regardless of
//! game-theory dynamics.

use crate::config::{MAX_TASKS, EVAL_FREQUENCY, STARVATION_THRESHOLD};
use crate::task::{TaskControlBlock, TaskState, TaskConfig, Strategy};
use crate::game::{self, SystemMetrics};

// ---------------------------------------------------------------------------
// Scheduler struct
// ---------------------------------------------------------------------------

/// The central scheduler state. Holds all task control blocks, system metrics,
/// and scheduling state. Stored as a global `static mut` in `kernel.rs`.
///
/// ## Design Notes
///
/// - All tasks are stored inline in a fixed-size array (no heap)
/// - `current_task` tracks the index of the currently running task
/// - An idle task (index 0) is always present as a fallback
pub struct Scheduler {
    /// Fixed-size array of TCBs. Index 0 is reserved for the idle task.
    pub tasks: [TaskControlBlock; MAX_TASKS],

    /// Index of the currently running task.
    pub current_task: usize,

    /// Number of allocated tasks (including idle task).
    pub task_count: usize,

    /// Aggregate system metrics for the game engine.
    pub metrics: SystemMetrics,

    /// Monotonic tick counter.
    pub tick_count: u64,

    /// Flag set by `tick()` when a reschedule is needed.
    pub needs_reschedule: bool,
}

impl Scheduler {
    /// Create a new scheduler with an idle task at index 0.
    pub const fn new() -> Self {
        Self {
            tasks: [TaskControlBlock::EMPTY; MAX_TASKS],
            current_task: 0,
            task_count: 0,
            metrics: SystemMetrics::new(),
            tick_count: 0,
            needs_reschedule: false,
        }
    }

    /// Register a new task with the scheduler.
    ///
    /// # Returns
    /// - `Ok(task_id)` — the index of the newly created task
    /// - `Err(())` — if the task array is full
    pub fn create_task(
        &mut self,
        entry: extern "C" fn() -> !,
        config: TaskConfig,
        strategy: Strategy,
    ) -> Result<usize, ()> {
        if self.task_count >= MAX_TASKS {
            return Err(());
        }

        let id = self.task_count;
        self.tasks[id].init(id, config, strategy);

        // Initialize the stack frame for context switching
        init_task_stack(&mut self.tasks[id], entry);

        self.task_count += 1;
        Ok(id)
    }

    /// Called from the SysTick handler every tick.
    ///
    /// Updates execution statistics, decrements time slices, and triggers
    /// periodic game evaluation. Sets `needs_reschedule` if a context
    /// switch should occur.
    pub fn tick(&mut self) {
        self.tick_count += 1;

        // --- Update current task metrics ---
        let current = self.current_task;
        if current < self.task_count && self.tasks[current].active {
            self.tasks[current].payoff.cpu_ticks_used += 1;
            self.tasks[current].total_ticks += 1;
            self.tasks[current].period_ticks += 1;

            // Decrement time slice
            if self.tasks[current].ticks_remaining > 0 {
                self.tasks[current].ticks_remaining -= 1;
            }

            // Time slice expired → yield to scheduler
            if self.tasks[current].ticks_remaining == 0 {
                self.tasks[current].state = TaskState::Ready;
                self.tasks[current].ticks_remaining =
                    self.tasks[current].config.effective_time_slice();

                // Check for WCET overrun
                if self.tasks[current].config.wcet_ticks > 0
                    && self.tasks[current].period_ticks > self.tasks[current].config.wcet_ticks
                {
                    self.tasks[current].record_overrun();
                }

                self.needs_reschedule = true;
            }
        }

        // --- Update starvation counters for non-running tasks ---
        for i in 0..self.task_count {
            if i != current && self.tasks[i].active && self.tasks[i].state == TaskState::Ready {
                self.tasks[i].payoff.ticks_since_last_run += 1;
            }
        }

        // --- Deadline checking for periodic tasks ---
        for i in 0..self.task_count {
            if !self.tasks[i].active {
                continue;
            }
            let deadline = self.tasks[i].config.deadline_ticks;
            if deadline > 0 && self.tasks[i].period_ticks >= deadline {
                if self.tasks[i].state == TaskState::Ready
                    || self.tasks[i].state == TaskState::Running
                {
                    // Task was still running/ready at deadline → missed
                    self.tasks[i].record_deadline_missed();
                }
                // Reset period counter
                self.tasks[i].period_ticks = 0;
            }
        }

        // --- Periodic game evaluation ---
        if self.tick_count % EVAL_FREQUENCY as u64 == 0 {
            self.evaluate_game();
        }
    }

    /// Run the game-theory evaluation engine.
    ///
    /// Recomputes payoff for each task, checks equilibrium, and
    /// updates strategies if the system is not in a stable state.
    fn evaluate_game(&mut self) {
        // Update system metrics
        self.update_system_metrics();

        // Recompute payoff for each active task
        for i in 0..self.task_count {
            if self.tasks[i].active {
                let payoff = game::compute_payoff(&self.tasks[i], &self.metrics);
                self.tasks[i].payoff.payoff = payoff;
            }
        }

        // Check equilibrium and update strategies if needed
        if !game::is_in_equilibrium(&self.tasks, self.task_count, &self.metrics) {
            game::update_strategies(&mut self.tasks, self.task_count, &self.metrics);
        }

        // Starvation prevention: boost starving tasks
        for i in 0..self.task_count {
            if self.tasks[i].active
                && self.tasks[i].payoff.ticks_since_last_run >= STARVATION_THRESHOLD
            {
                // Temporary payoff boost to ensure execution
                self.tasks[i].payoff.payoff += 500;
                self.needs_reschedule = true;
            }
        }
    }

    /// Update aggregate system metrics for the game engine.
    fn update_system_metrics(&mut self) {
        self.metrics.total_ticks = self.tick_count;

        let mut active = 0u32;
        let mut cooperative = 0u32;

        for i in 0..self.task_count {
            if self.tasks[i].active {
                active += 1;
                if self.tasks[i].strategy == Strategy::Cooperative {
                    cooperative += 1;
                }
            }
        }

        self.metrics.active_tasks = active;
        self.metrics.global_cooperation_ratio = if active > 0 {
            cooperative * 100 / active
        } else {
            100
        };

        // Overload: more ready tasks than cores can serve
        self.metrics.overload = active > crate::config::MAX_CORES as u32;
    }

    /// Select the next task to run.
    ///
    /// Picks the highest effective-priority runnable task that can run on core 0.
    /// Effective priority = base priority + payoff-adjusted weight.
    ///
    /// If no task is runnable, returns 0 (the idle task).
    ///
    /// # Returns
    /// Index of the next task to run.
    pub fn schedule(&mut self) -> usize {
        let mut best_task: usize = 0; // fallback to idle/first task
        let mut best_priority: i32 = i32::MIN;

        for i in 0..self.task_count {
            if !self.tasks[i].is_runnable() {
                continue;
            }

            // Must be able to run on core 0 (single-core)
            if !self.tasks[i].can_run_on_core(0) {
                continue;
            }

            let eff_prio = self.tasks[i].effective_priority();

            // Starvation boost: add extra priority weight for starving tasks
            let starvation_boost = if self.tasks[i].payoff.ticks_since_last_run >= STARVATION_THRESHOLD {
                (self.tasks[i].payoff.ticks_since_last_run / STARVATION_THRESHOLD) as i32 * 2
            } else {
                0
            };

            let total_prio = eff_prio + starvation_boost;

            if total_prio > best_priority {
                best_priority = total_prio;
                best_task = i;
            }
        }

        // Mark previous task as Ready (if it was Running)
        let prev = self.current_task;
        if prev < self.task_count && self.tasks[prev].state == TaskState::Running {
            self.tasks[prev].state = TaskState::Ready;
        }

        // Mark new task as Running
        if best_task < self.task_count {
            self.tasks[best_task].state = TaskState::Running;
            self.tasks[best_task].payoff.ticks_since_last_run = 0;
        }

        self.current_task = best_task;
        self.needs_reschedule = false;

        best_task
    }

    /// Record a voluntary yield from the current task.
    ///
    /// Called from `kernel::yield_task()`. Marks the current task as Ready,
    /// records the yield in payoff metrics, and triggers rescheduling.
    pub fn yield_current(&mut self) {
        let current = self.current_task;
        if current < self.task_count && self.tasks[current].active {
            self.tasks[current].state = TaskState::Ready;
            self.tasks[current].record_yield();
            self.tasks[current].ticks_remaining =
                self.tasks[current].config.effective_time_slice();
            self.needs_reschedule = true;
        }
    }

    /// Get a reference to the current task's TCB.
    pub fn current_tcb(&self) -> &TaskControlBlock {
        &self.tasks[self.current_task]
    }

    /// Get a mutable reference to the current task's TCB.
    pub fn current_tcb_mut(&mut self) -> &mut TaskControlBlock {
        &mut self.tasks[self.current_task]
    }
}

// ---------------------------------------------------------------------------
// Stack initialization helper
// ---------------------------------------------------------------------------

/// Initialize a task's stack frame for first-time context switch.
///
/// The Cortex-M4 hardware automatically pushes an exception frame on
/// interrupt entry. We pre-populate this frame on the task's stack so
/// that the first PendSV "return" starts executing the task function.
///
/// ## Stack Layout (top = high address, growing down)
///
/// ```text
/// [Hardware stacked frame]   <- initial PSP points here
///   xPSR  (Thumb bit set)
///   PC    (task entry point)
///   LR    (task_exit)
///   R12   (0)
///   R3    (0)
///   R2    (0)
///   R1    (0)
///   R0    (0)
/// [Software saved context]
///   R11   (0)
///   R10   (0)
///   R9    (0)
///   R8    (0)
///   R7    (0)
///   R6    (0)
///   R5    (0)
///   R4    (0)              <- stack_pointer after init
/// ```
fn init_task_stack(tcb: &mut TaskControlBlock, entry: extern "C" fn() -> !) {
    let stack_top = tcb.stack.as_ptr() as usize + STACK_SIZE;
    // Align to 8 bytes (AAPCS requirement)
    let aligned_top = stack_top & !0x07;

    // We need space for 16 registers (8 HW + 8 SW)
    let frame_ptr = (aligned_top - 16 * 4) as *mut u32;

    unsafe {
        // Software-saved registers (R4–R11) — bottom of frame
        for i in 0..8 {
            *frame_ptr.add(i) = 0; // R4, R5, R6, R7, R8, R9, R10, R11
        }

        // Hardware-stacked frame (R0–R3, R12, LR, PC, xPSR)
        *frame_ptr.add(8) = 0;  // R0
        *frame_ptr.add(9) = 0;  // R1
        *frame_ptr.add(10) = 0; // R2
        *frame_ptr.add(11) = 0; // R3
        *frame_ptr.add(12) = 0; // R12
        *frame_ptr.add(13) = task_exit as u32; // LR — return address if task returns
        *frame_ptr.add(14) = entry as u32;     // PC — task entry point
        *frame_ptr.add(15) = 0x0100_0000;      // xPSR — Thumb bit set
    }

    tcb.stack_pointer = frame_ptr;
}

/// Fallback for tasks that return (they shouldn't — entry is `fn() -> !`).
/// Loops forever to prevent undefined behavior.
extern "C" fn task_exit() -> ! {
    loop {
        cortex_m::asm::wfi();
    }
}
