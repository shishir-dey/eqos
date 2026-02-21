//! # EqOS Configuration
//!
//! Compile-time constants governing the scheduler and system behavior.
//! All limits are fixed at compile time — no dynamic allocation.

/// Maximum number of tasks the system can manage simultaneously.
/// This bounds the static TCB array. Increase with care — each task
/// consumes `STACK_SIZE` bytes of RAM.
pub const MAX_TASKS: usize = 8;

/// SysTick frequency in Hz. Determines scheduler tick granularity.
/// Higher values give finer scheduling precision at the cost of
/// increased interrupt overhead.
pub const TICK_HZ: u32 = 1000;

/// Default time slice in ticks. A task runs for this many ticks
/// before the scheduler re-evaluates. The game engine may adjust
/// effective slices via payoff weighting.
pub const DEFAULT_TIME_SLICE: u32 = 10;

/// Per-task stack size in bytes. Must be large enough for the
/// deepest call chain plus the hardware exception frame (32 bytes)
/// and the software-saved context (32 bytes for R4–R11).
pub const STACK_SIZE: usize = 1024;

/// Number of processor cores. Set to 1 for Cortex-M4 (single-core).
/// The architecture is designed to be extensible to multi-core systems
/// by increasing this value and implementing per-core scheduling.
pub const MAX_CORES: usize = 1;

/// Number of ticks a task can receive zero CPU before the starvation
/// prevention mechanism triggers a priority boost.
pub const STARVATION_THRESHOLD: u32 = 50;

/// Number of consecutive evaluation windows with declining payoff
/// required before a task switches strategy. Provides hysteresis
/// to avoid oscillation.
pub const STRATEGY_HYSTERESIS: u32 = 3;

/// Game evaluation frequency divisor. The full equilibrium check
/// runs every `EVAL_FREQUENCY` ticks to bound overhead.
/// Payoff incremental updates still occur every tick.
pub const EVAL_FREQUENCY: u32 = 10;

/// System clock frequency in Hz (default for STM32F4 at 16 MHz HSI).
pub const SYSTEM_CLOCK_HZ: u32 = 16_000_000;
