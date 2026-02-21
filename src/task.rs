//! # Task Control Block
//!
//! Defines the task model for EqOS. Each task is a rational agent in the
//! scheduling game, with its own strategy, payoff history, and execution state.
//!
//! ## Game-Theory Model
//!
//! Tasks operate under a Prisoner's Dilemma framework:
//! - **Cooperative** tasks voluntarily yield CPU time, respect soft deadlines,
//!   and share resources fairly. They receive payoff bonuses.
//! - **Selfish** tasks maximize their own CPU consumption without regard for
//!   others. They receive short-term gains but long-term penalties.
//!
//! The scheduler uses payoff metrics to weight scheduling priority, driving
//! the system toward Nash equilibrium where no task benefits from unilaterally
//! changing its strategy.

use crate::config::{STACK_SIZE, DEFAULT_TIME_SLICE};

// ---------------------------------------------------------------------------
// Task state machine
// ---------------------------------------------------------------------------

/// Execution state of a task in the scheduler's state machine.
///
/// ```text
///   ┌──────────┐     schedule()      ┌─────────┐
///   │  Ready   │ ──────────────────► │ Running │
///   └──────────┘                     └─────────┘
///        ▲                                │
///        │         preempt / yield        │
///        └───────────────────────────────┘
///        │                                │
///        │         block()               ▼
///        │                          ┌──────────┐
///        └───────────────────────── │ Blocked  │
///                  unblock()        └──────────┘
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// Task is ready to run and waiting in the run queue.
    Ready,
    /// Task is currently executing on the CPU.
    Running,
    /// Task is blocked waiting for an event or resource.
    Blocked,
    /// Task is suspended by the kernel (not schedulable).
    Suspended,
    /// Task has completed execution and will not be scheduled again.
    Terminated,
}

// ---------------------------------------------------------------------------
// Strategy model
// ---------------------------------------------------------------------------

/// Behavioral strategy of a task in the scheduling game.
///
/// This models the task's current "move" in the iterated Prisoner's Dilemma.
/// The scheduler observes task behavior and may override this based on
/// actual runtime metrics (e.g., a task claiming to be cooperative but
/// consuming excessive CPU will be reclassified).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Task cooperates: yields voluntarily, respects soft deadlines,
    /// uses only its fair share of CPU. Receives cooperation bonuses.
    Cooperative,
    /// Task defects: maximizes CPU consumption, ignores yielding hints,
    /// may overrun time slices. Receives short-term priority but
    /// accumulates penalties over time.
    Selfish,
}

// ---------------------------------------------------------------------------
// Task configuration (immutable after creation)
// ---------------------------------------------------------------------------

/// Static configuration for a task, set at creation time.
///
/// These parameters define the task's scheduling constraints and are
/// immutable during task execution. The scheduler uses them alongside
/// dynamic payoff metrics to make scheduling decisions.
#[derive(Debug, Clone, Copy)]
pub struct TaskConfig {
    /// Base priority (higher = more important). Range: 0–255.
    /// This is the static priority before game-theory adjustments.
    pub priority: u8,

    /// Deadline in ticks from the start of each period.
    /// `0` means no deadline constraint (best-effort task).
    /// The payoff function rewards meeting deadlines and penalizes misses.
    pub deadline_ticks: u32,

    /// Worst-case execution time in ticks.
    /// Used for overrun detection: if a task exceeds its WCET,
    /// consecutive overrun penalties are applied.
    pub wcet_ticks: u32,

    /// CPU affinity bitmask. Bit `i` set means the task may run on core `i`.
    /// For single-core Cortex-M4, this should be `0x01`.
    /// Extensible to multi-core by setting multiple bits.
    pub affinity_mask: u32,

    /// Time slice in ticks for this task. If 0, uses `DEFAULT_TIME_SLICE`.
    pub time_slice: u32,
}

impl TaskConfig {
    /// Returns the effective time slice, falling back to the system default.
    #[inline]
    pub const fn effective_time_slice(&self) -> u32 {
        if self.time_slice > 0 {
            self.time_slice
        } else {
            DEFAULT_TIME_SLICE
        }
    }
}

// ---------------------------------------------------------------------------
// Payoff metrics (mutable, updated every tick)
// ---------------------------------------------------------------------------

/// Runtime metrics tracked by the game engine to compute a task's payoff.
///
/// All values use integer arithmetic (no floating point) for determinism
/// and Cortex-M4 compatibility. The `cooperation_score` uses fixed-point
/// representation: value × 100 (e.g., 150 = 1.50).
///
/// ## Payoff Computation
///
/// The scheduler evaluates these metrics every `EVAL_FREQUENCY` ticks
/// and computes a composite payoff score that adjusts the task's effective
/// scheduling priority.
#[derive(Debug, Clone, Copy)]
pub struct PayoffMetrics {
    /// Total CPU ticks consumed by this task since last reset.
    pub cpu_ticks_used: u32,

    /// Number of deadlines successfully met.
    pub deadlines_met: u32,

    /// Number of deadlines missed.
    pub deadlines_missed: u32,

    /// Number of voluntary yields (cooperative behavior indicator).
    pub voluntary_yields: u32,

    /// Number of time-slice overruns (consecutive tracked separately).
    pub overruns: u32,

    /// Current consecutive overrun count. Reset on normal completion.
    /// Consecutive overruns incur escalating penalties.
    pub consecutive_overruns: u32,

    /// Cooperation score in fixed-point (×100).
    /// Starts at 100 (neutral). Increases for cooperative behavior,
    /// decreases for selfish behavior. Range: 0–500.
    pub cooperation_score: i32,

    /// Composite payoff value computed by the game engine.
    /// Higher values mean better scheduling treatment.
    pub payoff: i32,

    /// Previous payoff value (for trend detection / hysteresis).
    pub previous_payoff: i32,

    /// Number of consecutive evaluation windows with declining payoff.
    /// Used for strategy-switch hysteresis.
    pub decline_streak: u32,

    /// Ticks since this task last received any CPU time.
    /// Used for starvation detection.
    pub ticks_since_last_run: u32,
}

impl PayoffMetrics {
    /// Create zeroed payoff metrics with neutral cooperation score.
    pub const fn new() -> Self {
        Self {
            cpu_ticks_used: 0,
            deadlines_met: 0,
            deadlines_missed: 0,
            voluntary_yields: 0,
            overruns: 0,
            consecutive_overruns: 0,
            cooperation_score: 100,
            payoff: 0,
            previous_payoff: 0,
            decline_streak: 0,
            ticks_since_last_run: 0,
        }
    }

    /// Reset all metrics to initial values. Called on task restart.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

// ---------------------------------------------------------------------------
// Task Control Block
// ---------------------------------------------------------------------------

/// Task Control Block (TCB) — the central data structure for each task.
///
/// Contains all state needed to schedule, context-switch, and evaluate
/// a task within the game-theory framework. TCBs are stored in a static
/// array in the scheduler — no heap allocation.
///
/// ## Memory Layout
///
/// Each TCB includes an inline stack (`[u8; STACK_SIZE]`). The
/// `stack_pointer` field points into this stack and is updated on
/// every context switch.
pub struct TaskControlBlock {
    /// Unique task identifier (index in the scheduler's task array).
    pub id: usize,

    /// Current execution state.
    pub state: TaskState,

    /// Static configuration (priority, deadline, WCET, affinity).
    pub config: TaskConfig,

    /// Current game-theory strategy.
    pub strategy: Strategy,

    /// Runtime payoff metrics for the game engine.
    pub payoff: PayoffMetrics,

    /// Saved stack pointer (PSP). Updated on context switch.
    /// Points into `self.stack`.
    pub stack_pointer: *mut u32,

    /// Per-task stack memory. Aligned to 8 bytes as required by ARM AAPCS.
    #[repr(align(8))]
    pub stack: [u8; STACK_SIZE],

    /// Remaining ticks in the current time slice.
    pub ticks_remaining: u32,

    /// Total ticks this task has been in the Running state.
    pub total_ticks: u32,

    /// Period tracking: ticks since the start of the current period.
    /// Used for deadline evaluation on periodic tasks.
    pub period_ticks: u32,

    /// Whether this task is allocated (true) or a free slot (false).
    pub active: bool,
}

// Safety: TaskControlBlock contains a raw pointer (stack_pointer) but
// it always points into the task's own stack array. We only access TCBs
// within critical sections.
unsafe impl Send for TaskControlBlock {}
unsafe impl Sync for TaskControlBlock {}

impl TaskControlBlock {
    /// Create an empty (unallocated) TCB. Used to initialize the static array.
    pub const fn empty() -> Self {
        Self {
            id: 0,
            state: TaskState::Suspended,
            config: TaskConfig {
                priority: 0,
                deadline_ticks: 0,
                wcet_ticks: 0,
                affinity_mask: 0x01,
                time_slice: 0,
            },
            strategy: Strategy::Cooperative,
            payoff: PayoffMetrics::new(),
            stack_pointer: core::ptr::null_mut(),
            stack: [0u8; STACK_SIZE],
            ticks_remaining: 0,
            total_ticks: 0,
            period_ticks: 0,
            active: false,
        }
    }

    /// Initialize a TCB for a new task with the given configuration and strategy.
    ///
    /// This sets the task to Ready state and initializes its time slice.
    /// The stack must be separately initialized by `arch::init_stack()`.
    pub fn init(&mut self, id: usize, config: TaskConfig, strategy: Strategy) {
        self.id = id;
        self.state = TaskState::Ready;
        self.config = config;
        self.strategy = strategy;
        self.payoff = PayoffMetrics::new();
        self.ticks_remaining = config.effective_time_slice();
        self.total_ticks = 0;
        self.period_ticks = 0;
        self.active = true;
    }

    /// Record that this task voluntarily yielded the CPU.
    /// Increments the yield counter and boosts cooperation score.
    pub fn record_yield(&mut self) {
        self.payoff.voluntary_yields += 1;
        // Boost cooperation score (capped at 500)
        self.payoff.cooperation_score = (self.payoff.cooperation_score + 10).min(500);
    }

    /// Record that this task met its deadline for the current period.
    pub fn record_deadline_met(&mut self) {
        self.payoff.deadlines_met += 1;
        self.payoff.consecutive_overruns = 0;
    }

    /// Record that this task missed its deadline.
    pub fn record_deadline_missed(&mut self) {
        self.payoff.deadlines_missed += 1;
    }

    /// Record a time-slice overrun.
    pub fn record_overrun(&mut self) {
        self.payoff.overruns += 1;
        self.payoff.consecutive_overruns += 1;
        // Reduce cooperation score (floored at 0)
        self.payoff.cooperation_score = (self.payoff.cooperation_score - 20).max(0);
    }

    /// Check if this task is runnable (Ready and active).
    #[inline]
    pub fn is_runnable(&self) -> bool {
        self.active && self.state == TaskState::Ready
    }

    /// Check if this task can run on the given core.
    #[inline]
    pub fn can_run_on_core(&self, core_id: u32) -> bool {
        (self.config.affinity_mask & (1 << core_id)) != 0
    }

    /// Get the effective priority after game-theory payoff adjustment.
    ///
    /// The payoff is scaled and added to the base priority. A task with
    /// high payoff gets a scheduling boost; one with negative payoff
    /// gets deprioritized (but never below 0).
    pub fn effective_priority(&self) -> i32 {
        let base = self.config.priority as i32;
        // Scale payoff: divide by 100 to convert from fixed-point
        let payoff_adjustment = self.payoff.payoff / 100;
        (base + payoff_adjustment).max(0)
    }
}

// ---------------------------------------------------------------------------
// Unit tests (host-only)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tcb_initialization() {
        let mut tcb = TaskControlBlock::empty();
        assert!(!tcb.active);
        assert_eq!(tcb.state, TaskState::Suspended);

        let config = TaskConfig {
            priority: 5,
            deadline_ticks: 100,
            wcet_ticks: 20,
            affinity_mask: 0x01,
            time_slice: 15,
        };
        tcb.init(0, config, Strategy::Cooperative);

        assert!(tcb.active);
        assert_eq!(tcb.state, TaskState::Ready);
        assert_eq!(tcb.config.priority, 5);
        assert_eq!(tcb.strategy, Strategy::Cooperative);
        assert_eq!(tcb.ticks_remaining, 15);
        assert_eq!(tcb.payoff.cooperation_score, 100);
    }

    #[test]
    fn test_yield_recording() {
        let mut tcb = TaskControlBlock::empty();
        let config = TaskConfig {
            priority: 3,
            deadline_ticks: 0,
            wcet_ticks: 10,
            affinity_mask: 0x01,
            time_slice: 0,
        };
        tcb.init(1, config, Strategy::Cooperative);

        tcb.record_yield();
        assert_eq!(tcb.payoff.voluntary_yields, 1);
        assert_eq!(tcb.payoff.cooperation_score, 110);

        // Score capped at 500
        tcb.payoff.cooperation_score = 495;
        tcb.record_yield();
        assert_eq!(tcb.payoff.cooperation_score, 500);
    }

    #[test]
    fn test_overrun_recording() {
        let mut tcb = TaskControlBlock::empty();
        let config = TaskConfig {
            priority: 3,
            deadline_ticks: 0,
            wcet_ticks: 10,
            affinity_mask: 0x01,
            time_slice: 0,
        };
        tcb.init(2, config, Strategy::Selfish);

        tcb.record_overrun();
        assert_eq!(tcb.payoff.overruns, 1);
        assert_eq!(tcb.payoff.consecutive_overruns, 1);
        assert_eq!(tcb.payoff.cooperation_score, 80);

        // Score floored at 0
        tcb.payoff.cooperation_score = 10;
        tcb.record_overrun();
        assert_eq!(tcb.payoff.cooperation_score, 0);
    }

    #[test]
    fn test_effective_priority() {
        let mut tcb = TaskControlBlock::empty();
        let config = TaskConfig {
            priority: 5,
            deadline_ticks: 0,
            wcet_ticks: 10,
            affinity_mask: 0x01,
            time_slice: 0,
        };
        tcb.init(3, config, Strategy::Cooperative);

        // With positive payoff
        tcb.payoff.payoff = 300;
        assert_eq!(tcb.effective_priority(), 8);

        // With negative payoff (floored at 0)
        tcb.payoff.payoff = -1000;
        assert_eq!(tcb.effective_priority(), 0);
    }

    #[test]
    fn test_affinity() {
        let mut tcb = TaskControlBlock::empty();
        let config = TaskConfig {
            priority: 3,
            deadline_ticks: 0,
            wcet_ticks: 10,
            affinity_mask: 0b0101, // cores 0 and 2
            time_slice: 0,
        };
        tcb.init(4, config, Strategy::Cooperative);

        assert!(tcb.can_run_on_core(0));
        assert!(!tcb.can_run_on_core(1));
        assert!(tcb.can_run_on_core(2));
    }

    #[test]
    fn test_effective_time_slice_default() {
        let config = TaskConfig {
            priority: 1,
            deadline_ticks: 0,
            wcet_ticks: 10,
            affinity_mask: 0x01,
            time_slice: 0,
        };
        assert_eq!(config.effective_time_slice(), DEFAULT_TIME_SLICE);
    }
}
