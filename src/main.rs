//! # EqOS Example Firmware
//!
//! Demonstrates the game-theory scheduler with 4 tasks exhibiting
//! different behavioral strategies:
//!
//! | Task | Type | Priority | Strategy | Behavior |
//! |------|------|----------|----------|----------|
//! | `cpu_bound_task` | CPU hog | 2 | Selfish | Busy-loops without yielding |
//! | `periodic_deadline_task` | Periodic | 3 | Cooperative | 100ms period, yields early |
//! | `cooperative_yielding_task` | Yielder | 1 | Cooperative | Yields frequently |
//! | `sporadic_high_prio_task` | Sporadic | 5 | Cooperative | Short bursts, then yields |
//!
//! ## Expected Game Dynamics
//!
//! 1. **Initial phase**: The CPU-bound selfish task grabs most CPU time,
//!    while cooperative tasks get starved.
//!
//! 2. **Payoff adjustment**: After several evaluation windows:
//!    - The selfish task's payoff drops due to CPU fairness penalties
//!      and lack of cooperation bonuses.
//!    - Cooperative tasks accumulate yield bonuses and cooperation multipliers.
//!    - Starvation prevention boosts cooperative tasks.
//!
//! 3. **Convergence**: The scheduler reaches steady state where:
//!    - The sporadic high-priority task runs immediately on bursts.
//!    - The periodic task meets its deadlines consistently.
//!    - The CPU-bound task is throttled to its fair share.
//!    - The yielding task gets consistent (if modest) CPU time.
//!    - If the selfish task switches to cooperative, it receives better
//!      long-term payoff — Nash equilibrium favors cooperation.

#![no_std]
#![no_main]

use cortex_m_rt::entry;
use panic_halt as _;

use eqos::kernel;
use eqos::task::{TaskConfig, Strategy};

// ---------------------------------------------------------------------------
// Task entry points
// ---------------------------------------------------------------------------

/// **CPU-Bound Selfish Task** (Priority 2)
///
/// Simulates a task that tries to monopolize the CPU. It never yields
/// voluntarily — the only way it gives up the CPU is through preemption
/// when its time slice expires.
///
/// **Game effect**: This task's payoff will steadily decline due to:
/// - CPU fairness penalty (consuming >2× fair share)
/// - No voluntary yield bonuses
/// - No cooperation multiplier
/// - Possible overrun penalties if WCET is set
///
/// Over time, the scheduler reduces its effective priority, giving
/// more CPU time to cooperative tasks.
extern "C" fn cpu_bound_task() -> ! {
    let mut _counter: u32 = 0;
    loop {
        // Pure CPU consumption — no yielding, no sleeping
        // This represents a computationally intensive workload
        // (e.g., signal processing, matrix multiplication)
        _counter = _counter.wrapping_add(1);

        // The task never calls kernel::yield_task(), so it will
        // only be preempted when its time slice expires.
        // This "selfish" behavior is observed by the game engine.
    }
}

/// **Periodic Deadline Task** (Priority 3)
///
/// Simulates a periodic real-time task with a 100ms deadline.
/// It does a small amount of work and then yields, completing
/// well within its deadline.
///
/// **Game effect**: This task accumulates:
/// - Deadline compliance bonuses (+100 per met deadline)
/// - Voluntary yield bonuses (+50 per yield)
/// - Cooperation multiplier (1.5× on positive payoff)
/// - Rising cooperation score
///
/// Its effective priority grows over time, ensuring reliable
/// deadline compliance even under contention.
extern "C" fn periodic_deadline_task() -> ! {
    loop {
        // Simulate periodic work (e.g., sensor sampling)
        // Do ~5ms of computation within a 100ms period
        let mut work: u32 = 0;
        for _ in 0..5000 {
            work = work.wrapping_add(1);
        }

        // Yield early — cooperative behavior
        // This signals to the game engine that the task is
        // cooperating by not consuming its full time slice.
        kernel::yield_task();
    }
}

/// **Cooperative Yielding Task** (Priority 1)
///
/// A low-priority background task that aggressively yields after
/// minimal work. Demonstrates the "ideal cooperative citizen" in
/// the game framework.
///
/// **Game effect**: Despite low base priority, this task's payoff
/// grows rapidly through yield bonuses and cooperation multiplier.
/// Its effective priority (base + payoff adjustment) eventually
/// ensures it gets reasonable CPU time, demonstrating that
/// cooperation is rewarded by the game engine.
extern "C" fn cooperative_yielding_task() -> ! {
    loop {
        // Minimal work
        let mut _x: u32 = 0;
        for _ in 0..100 {
            _x = _x.wrapping_add(1);
        }

        // Yield immediately — maximum cooperation
        kernel::yield_task();
    }
}

/// **Sporadic High-Priority Task** (Priority 5)
///
/// Simulates an interrupt-driven or event-driven task with
/// sporadic activation. It does a short burst of work and then
/// yields, simulating event processing completion.
///
/// **Game effect**: High base priority ensures immediate response
/// to events. Cooperative yielding after short bursts compounds
/// the priority advantage with payoff bonuses. This task
/// demonstrates that high-priority tasks can also cooperate —
/// they don't need to be selfish to maintain responsiveness.
extern "C" fn sporadic_high_prio_task() -> ! {
    loop {
        // Simulate a burst of event-driven work
        // (e.g., processing a received packet, handling a button press)
        let mut _result: u32 = 0;
        for _ in 0..2000 {
            _result = _result.wrapping_add(1);
        }

        // Done processing — yield until next event
        kernel::yield_task();
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Firmware entry point. Initializes the kernel, creates tasks, and
/// starts the game-theory scheduler. Does not return.
#[entry]
fn main() -> ! {
    // Take ownership of core peripherals
    let cp = cortex_m::Peripherals::take().unwrap();

    // Initialize the EqOS kernel
    kernel::init();

    // --- Create tasks ---

    // Task 0: CPU-bound selfish task
    kernel::create_task(
        cpu_bound_task,
        TaskConfig {
            priority: 2,
            deadline_ticks: 0,          // No deadline (best-effort)
            wcet_ticks: 15,             // 15-tick WCET
            affinity_mask: 0x01,        // Core 0
            time_slice: 10,             // Standard slice
        },
        Strategy::Selfish,
    ).expect("Failed to create cpu_bound_task");

    // Task 1: Periodic deadline task (100ms period at 1kHz = 100 ticks)
    kernel::create_task(
        periodic_deadline_task,
        TaskConfig {
            priority: 3,
            deadline_ticks: 100,        // 100ms deadline
            wcet_ticks: 5,              // 5ms WCET
            affinity_mask: 0x01,
            time_slice: 10,
        },
        Strategy::Cooperative,
    ).expect("Failed to create periodic_deadline_task");

    // Task 2: Cooperative yielding task
    kernel::create_task(
        cooperative_yielding_task,
        TaskConfig {
            priority: 1,
            deadline_ticks: 0,          // No deadline
            wcet_ticks: 0,              // No WCET constraint
            affinity_mask: 0x01,
            time_slice: 10,
        },
        Strategy::Cooperative,
    ).expect("Failed to create cooperative_yielding_task");

    // Task 3: Sporadic high-priority task
    kernel::create_task(
        sporadic_high_prio_task,
        TaskConfig {
            priority: 5,
            deadline_ticks: 50,         // 50ms response deadline
            wcet_ticks: 3,              // 3ms WCET
            affinity_mask: 0x01,
            time_slice: 5,              // Shorter slice for responsiveness
        },
        Strategy::Cooperative,
    ).expect("Failed to create sporadic_high_prio_task");

    // Start the scheduler — does not return
    kernel::start(cp)
}
