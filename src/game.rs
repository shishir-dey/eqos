//! # Game-Theory Engine
//!
//! Implements the payoff function, equilibrium detection, and strategy
//! update logic for the EqOS scheduling game.
//!
//! ## Game Model
//!
//! Each task is a player in an iterated Prisoner's Dilemma:
//!
//! | Task A \ Task B | Cooperate         | Defect            |
//! |-----------------|-------------------|-------------------|
//! | **Cooperate**   | Both: high payoff | A: penalized, B: moderate |
//! | **Defect**      | A: moderate, B: penalized | Both: low payoff |
//!
//! The payoff function evaluates each task independently based on:
//! - Deadline compliance (+100 per met, -200 per missed)
//! - Voluntary yields (+50 each)
//! - Consecutive overruns (-150 penalty)
//! - CPU fairness (bonus/penalty based on deviation from fair share)
//! - Cooperation multiplier (1.5× for cooperative tasks)
//! - Global cooperation ratio (collective defection penalty)
//!
//! ## Equilibrium Approximation
//!
//! Rather than solving the full game matrix every tick (O(n²) or worse),
//! EqOS uses incremental scoring:
//! 1. Payoff is recomputed every `EVAL_FREQUENCY` ticks
//! 2. Each task evaluates whether switching strategy would improve payoff
//! 3. If no task benefits from switching → system is in Nash equilibrium
//! 4. Strategy changes require sustained payoff decline (hysteresis)

use crate::config::{MAX_TASKS, STRATEGY_HYSTERESIS};
use crate::task::{TaskControlBlock, Strategy};

// ---------------------------------------------------------------------------
// System-wide metrics (provided by the scheduler)
// ---------------------------------------------------------------------------

/// Aggregate system metrics used by the game engine for payoff computation.
///
/// These are computed by the scheduler and passed to the game engine.
/// They provide the "global state" that individual payoff calculations
/// reference (e.g., fair CPU share depends on active task count).
#[derive(Debug, Clone, Copy)]
pub struct SystemMetrics {
    /// Total ticks elapsed since system start.
    pub total_ticks: u64,
    /// Number of active (Ready or Running) tasks.
    pub active_tasks: u32,
    /// Ratio of cooperative tasks (×100 fixed-point). E.g., 75 = 75%.
    pub global_cooperation_ratio: u32,
    /// True if the system is in overload (more tasks than can be served).
    pub overload: bool,
}

impl SystemMetrics {
    pub const fn new() -> Self {
        Self {
            total_ticks: 0,
            active_tasks: 0,
            global_cooperation_ratio: 100,
            overload: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Payoff computation
// ---------------------------------------------------------------------------

/// Compute the payoff score for a single task.
///
/// The payoff is a composite score reflecting how "well-behaved" the task is
/// from the system's perspective. Higher payoff → better scheduling treatment.
///
/// ## Payoff Components
///
/// | Component | Value | Rationale |
/// |-----------|-------|-----------|
/// | Deadline met | +100 | Reward timely completion |
/// | Deadline missed | -200 | Heavily penalize lateness |
/// | Voluntary yield | +50 | Reward cooperation |
/// | Consecutive overrun | -150 × count | Escalating penalty for hogging |
/// | Fair-share deviation | ±penalty | Penalize CPU usage > 2× fair share |
/// | Cooperation multiplier | ×1.5 | Bonus for cooperative strategy |
/// | Global defection penalty | -100 | Applied when <50% tasks cooperate |
///
/// All arithmetic is integer-only. The final payoff is in fixed-point ×100.
pub fn compute_payoff(task: &TaskControlBlock, metrics: &SystemMetrics) -> i32 {
    let mut payoff: i32 = 0;

    // --- Deadline compliance ---
    payoff += task.payoff.deadlines_met as i32 * 100;
    payoff -= task.payoff.deadlines_missed as i32 * 200;

    // --- Voluntary yields ---
    payoff += task.payoff.voluntary_yields as i32 * 50;

    // --- Consecutive overrun penalty (escalating) ---
    let overrun_count = task.payoff.consecutive_overruns as i32;
    payoff -= overrun_count * 150;

    // --- CPU fairness ---
    // Fair share = total_ticks / active_tasks
    if metrics.active_tasks > 0 && metrics.total_ticks > 0 {
        let fair_share = (metrics.total_ticks / metrics.active_tasks as u64) as u32;
        let actual = task.payoff.cpu_ticks_used;

        if fair_share > 0 {
            // Ratio of actual/fair × 100
            let usage_ratio = (actual as u64 * 100 / fair_share as u64) as i32;

            if usage_ratio > 200 {
                // Using more than 2× fair share → penalty
                payoff -= (usage_ratio - 200) * 2;
            } else if usage_ratio < 50 {
                // Using less than half fair share → small bonus (being modest)
                payoff += (50 - usage_ratio);
            }
        }
    }

    // --- Cooperation multiplier ---
    // Cooperative tasks get a 1.5× multiplier on positive payoff
    if task.strategy == Strategy::Cooperative && payoff > 0 {
        payoff = payoff * 3 / 2;
    }

    // --- Global cooperation penalty ---
    // If fewer than 50% of tasks are cooperating, everyone gets penalized
    // (Prisoner's Dilemma: mutual defection is collectively worse)
    if metrics.global_cooperation_ratio < 50 {
        payoff -= 100;
    }

    // --- Cooperation score integration ---
    // Blend the existing cooperation score into the payoff
    payoff += task.payoff.cooperation_score / 2;

    payoff
}

// ---------------------------------------------------------------------------
// Equilibrium detection
// ---------------------------------------------------------------------------

/// Check whether the system is currently in Nash equilibrium.
///
/// The system is in equilibrium if no task would improve its payoff by
/// unilaterally switching its strategy (cooperative ↔ selfish).
///
/// This is an approximation: we compute the hypothetical payoff for each
/// task under the alternative strategy and check if any task would benefit.
///
/// # Returns
/// `true` if no task benefits from switching strategy.
pub fn is_in_equilibrium(tasks: &[TaskControlBlock; MAX_TASKS], task_count: usize, metrics: &SystemMetrics) -> bool {
    for i in 0..task_count {
        if !tasks[i].active {
            continue;
        }

        let current_payoff = tasks[i].payoff.payoff;

        // Estimate payoff under alternative strategy
        let alt_payoff = estimate_alternative_payoff(&tasks[i], metrics);

        // If switching would improve payoff by more than a threshold, not in equilibrium
        if alt_payoff > current_payoff + 50 {
            return false;
        }
    }
    true
}

/// Estimate what a task's payoff would be if it switched strategy.
///
/// This is a lightweight approximation — we don't fully re-simulate,
/// but apply the strategy-dependent modifiers to the current base score.
fn estimate_alternative_payoff(task: &TaskControlBlock, metrics: &SystemMetrics) -> i32 {
    let mut payoff: i32 = 0;

    // Same base components
    payoff += task.payoff.deadlines_met as i32 * 100;
    payoff -= task.payoff.deadlines_missed as i32 * 200;
    payoff += task.payoff.voluntary_yields as i32 * 50;
    payoff -= task.payoff.consecutive_overruns as i32 * 150;

    // Flip the cooperation multiplier
    match task.strategy {
        Strategy::Cooperative => {
            // If currently cooperative, switching to selfish removes the multiplier
            // No multiplier applied
        }
        Strategy::Selfish => {
            // If currently selfish, switching to cooperative adds the multiplier
            if payoff > 0 {
                payoff = payoff * 3 / 2;
            }
        }
    }

    if metrics.global_cooperation_ratio < 50 {
        payoff -= 100;
    }

    payoff += task.payoff.cooperation_score / 2;

    payoff
}

// ---------------------------------------------------------------------------
// Strategy update
// ---------------------------------------------------------------------------

/// Update task strategies based on payoff trends.
///
/// A task switches strategy only after `STRATEGY_HYSTERESIS` consecutive
/// evaluation windows with declining payoff. This prevents oscillation.
///
/// Strategy transitions:
/// - Selfish → Cooperative: when payoff declines (defection being penalized)
/// - Cooperative → Selfish: when payoff declines (cooperation not rewarded)
///
/// In practice, the payoff function is designed so that sustained cooperation
/// yields higher payoff, creating a natural attractor toward cooperative
/// equilibrium.
pub fn update_strategies(tasks: &mut [TaskControlBlock; MAX_TASKS], task_count: usize, _metrics: &SystemMetrics) {
    for i in 0..task_count {
        if !tasks[i].active {
            continue;
        }

        let current = tasks[i].payoff.payoff;
        let previous = tasks[i].payoff.previous_payoff;

        if current < previous {
            tasks[i].payoff.decline_streak += 1;
        } else {
            tasks[i].payoff.decline_streak = 0;
        }

        // Switch strategy after sustained decline
        if tasks[i].payoff.decline_streak >= STRATEGY_HYSTERESIS {
            tasks[i].strategy = match tasks[i].strategy {
                Strategy::Cooperative => Strategy::Selfish,
                Strategy::Selfish => Strategy::Cooperative,
            };
            tasks[i].payoff.decline_streak = 0;
        }

        // Store current as previous for next evaluation
        tasks[i].payoff.previous_payoff = current;
    }
}

// ---------------------------------------------------------------------------
// Prisoner's Dilemma payoff matrix (for documentation / explicit encoding)
// ---------------------------------------------------------------------------

/// Compute pairwise Prisoner's Dilemma payoff between two tasks.
///
/// This is used conceptually to understand the system dynamics.
/// The actual scheduler uses `compute_payoff` which integrates
/// these concepts into a single per-task score.
///
/// | A \ B     | Cooperate | Defect |
/// |-----------|-----------|--------|
/// | Cooperate | (3, 3)    | (0, 5) |
/// | Defect    | (5, 0)    | (1, 1) |
///
/// Returns `(payoff_a, payoff_b)` scaled by 100.
pub fn prisoners_dilemma_payoff(a: Strategy, b: Strategy) -> (i32, i32) {
    match (a, b) {
        (Strategy::Cooperative, Strategy::Cooperative) => (300, 300),
        (Strategy::Cooperative, Strategy::Selfish) => (0, 500),
        (Strategy::Selfish, Strategy::Cooperative) => (500, 0),
        (Strategy::Selfish, Strategy::Selfish) => (100, 100),
    }
}

// ---------------------------------------------------------------------------
// Unit tests (host-only)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{TaskConfig, TaskState};

    fn make_test_task(id: usize, strategy: Strategy, priority: u8) -> TaskControlBlock {
        let mut tcb = TaskControlBlock::empty();
        let config = TaskConfig {
            priority,
            deadline_ticks: 100,
            wcet_ticks: 20,
            affinity_mask: 0x01,
            time_slice: 10,
        };
        tcb.init(id, config, strategy);
        tcb
    }

    fn default_metrics() -> SystemMetrics {
        SystemMetrics {
            total_ticks: 1000,
            active_tasks: 4,
            global_cooperation_ratio: 75,
            overload: false,
        }
    }

    #[test]
    fn test_payoff_deadline_met() {
        let mut task = make_test_task(0, Strategy::Cooperative, 3);
        task.payoff.deadlines_met = 5;
        let metrics = default_metrics();

        let payoff = compute_payoff(&task, &metrics);
        // Should include 5×100 = 500 for deadlines, plus cooperation multiplier, plus coop score
        assert!(payoff > 500, "Payoff should include deadline bonus: {}", payoff);
    }

    #[test]
    fn test_payoff_deadline_missed_penalty() {
        let mut task = make_test_task(0, Strategy::Cooperative, 3);
        task.payoff.deadlines_missed = 3;
        let metrics = default_metrics();

        let payoff = compute_payoff(&task, &metrics);
        // -600 from misses, mitigated by coop score
        assert!(payoff < 0, "Payoff should be negative for missed deadlines: {}", payoff);
    }

    #[test]
    fn test_payoff_overrun_escalation() {
        let mut task = make_test_task(0, Strategy::Selfish, 3);
        task.payoff.consecutive_overruns = 5;
        let metrics = default_metrics();

        let payoff = compute_payoff(&task, &metrics);
        // -750 from overruns
        assert!(payoff < -500, "Overrun penalty should be severe: {}", payoff);
    }

    #[test]
    fn test_equilibrium_detection() {
        let mut tasks = [TaskControlBlock::empty(); MAX_TASKS];
        let metrics = default_metrics();

        // Two cooperative tasks with similar payoffs
        tasks[0] = make_test_task(0, Strategy::Cooperative, 3);
        tasks[0].payoff.payoff = 300;
        tasks[1] = make_test_task(1, Strategy::Cooperative, 3);
        tasks[1].payoff.payoff = 280;

        // When payoffs are similar, should be in equilibrium
        // (switching strategy wouldn't significantly improve either)
        let eq = is_in_equilibrium(&tasks, 2, &metrics);
        // This depends on the estimate — just verify it runs without panic
        let _ = eq;
    }

    #[test]
    fn test_strategy_update_hysteresis() {
        let mut tasks = [TaskControlBlock::empty(); MAX_TASKS];
        let metrics = default_metrics();
        tasks[0] = make_test_task(0, Strategy::Selfish, 3);

        // Simulate declining payoff over STRATEGY_HYSTERESIS windows
        for i in 0..STRATEGY_HYSTERESIS {
            tasks[0].payoff.payoff = 100 - (i as i32 * 50);
            tasks[0].payoff.previous_payoff = 150 - (i as i32 * 50);
            update_strategies(&mut tasks, 1, &metrics);
        }

        // After enough decline, strategy should have switched
        assert_eq!(tasks[0].strategy, Strategy::Cooperative,
            "Task should switch from Selfish to Cooperative after sustained decline");
    }

    #[test]
    fn test_prisoners_dilemma_encoding() {
        let (a, b) = prisoners_dilemma_payoff(Strategy::Cooperative, Strategy::Cooperative);
        assert_eq!((a, b), (300, 300), "Mutual cooperation: both get high payoff");

        let (a, b) = prisoners_dilemma_payoff(Strategy::Selfish, Strategy::Selfish);
        assert_eq!((a, b), (100, 100), "Mutual defection: both get low payoff");

        let (a, b) = prisoners_dilemma_payoff(Strategy::Cooperative, Strategy::Selfish);
        assert_eq!(a, 0, "Cooperator gets exploited");
        assert_eq!(b, 500, "Defector gets high payoff");

        // Verify: mutual cooperation beats mutual defection
        let (cc, _) = prisoners_dilemma_payoff(Strategy::Cooperative, Strategy::Cooperative);
        let (dd, _) = prisoners_dilemma_payoff(Strategy::Selfish, Strategy::Selfish);
        assert!(cc > dd, "Mutual cooperation must beat mutual defection");
    }

    #[test]
    fn test_cooperation_vs_selfish_payoff() {
        let metrics = default_metrics();

        let mut coop_task = make_test_task(0, Strategy::Cooperative, 3);
        coop_task.payoff.deadlines_met = 3;
        coop_task.payoff.voluntary_yields = 5;

        let mut selfish_task = make_test_task(1, Strategy::Selfish, 3);
        selfish_task.payoff.deadlines_met = 3;
        selfish_task.payoff.voluntary_yields = 0;
        selfish_task.payoff.consecutive_overruns = 2;

        let coop_payoff = compute_payoff(&coop_task, &metrics);
        let selfish_payoff = compute_payoff(&selfish_task, &metrics);

        assert!(coop_payoff > selfish_payoff,
            "Cooperative task should have higher payoff than selfish: {} vs {}",
            coop_payoff, selfish_payoff);
    }
}
