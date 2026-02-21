//! # EqOS — Equilibrium Operating System
//!
//! A game-theory-based Real-Time Operating System (RTOS) scheduler for
//! ARM Cortex-M4 microcontrollers.
//!
//! ## Overview
//!
//! EqOS models task scheduling as a strategic game inspired by Nash equilibrium
//! and the Prisoner's Dilemma. Each task is a rational agent competing for CPU
//! time, and the scheduler drives the system toward a stable equilibrium where:
//!
//! - **No task can improve its payoff by unilaterally changing its behavior**
//! - **System-wide performance remains stable and predictable**
//! - **Cooperative tasks are rewarded; selfish tasks are penalized**
//!
//! ## Architecture
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │                    Application Tasks                    │
//! ├────────────────────────────────────────────────────────┤
//! │                 Kernel API (kernel.rs)                  │
//! │          init() · create_task() · start() · yield()    │
//! ├──────────────┬────────────────────┬───────────────────┤
//! │  Scheduler   │   Game Engine      │  Sync Primitives  │
//! │  scheduler.rs│   game.rs          │  sync.rs          │
//! │  ─ tick()    │   ─ payoff()       │  ─ critical_section│
//! │  ─ schedule()│   ─ equilibrium()  │                   │
//! │  ─ yield()   │   ─ strategy()     │                   │
//! ├──────────────┴────────────────────┴───────────────────┤
//! │              Task Model (task.rs)                       │
//! │    TCB · Strategy · PayoffMetrics · TaskState           │
//! ├────────────────────────────────────────────────────────┤
//! │            Arch Port (arch/cortex_m4.rs)                │
//! │    PendSV · SysTick · Context Switch · Stack Init      │
//! ├────────────────────────────────────────────────────────┤
//! │         ARM Cortex-M4 Hardware (Thumb-2)                │
//! └────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Game-Theory Model
//!
//! ### Prisoner's Dilemma
//!
//! Tasks interact through an iterated Prisoner's Dilemma:
//!
//! | Task A ╲ B  | Cooperate | Defect |
//! |-------------|-----------|--------|
//! | **Cooperate** | (3,3)   | (0,5)  |
//! | **Defect**    | (5,0)   | (1,1)  |
//!
//! Mutual cooperation yields the highest collective payoff, but individual
//! defection tempts short-term gains. The scheduler penalizes sustained
//! defection, making cooperation the dominant long-term strategy.
//!
//! ### Payoff Function
//!
//! Each task's payoff is computed from:
//! - **Deadline compliance** (+100 met, -200 missed)
//! - **Voluntary yields** (+50 each)
//! - **Overrun penalties** (-150 × consecutive count)
//! - **CPU fairness** (penalty for >2× fair share)
//! - **Cooperation multiplier** (1.5× for cooperative tasks)
//!
//! ### Nash Equilibrium
//!
//! The system approximates Nash equilibrium incrementally:
//! 1. Payoffs are recomputed every `EVAL_FREQUENCY` ticks
//! 2. Each task evaluates if switching strategy would improve payoff
//! 3. Strategy changes require sustained decline (hysteresis)
//! 4. Equilibrium ≈ no task benefits from unilateral strategy change
//!
//! ## Memory Model
//!
//! - **No heap**: All state is statically allocated
//! - **No `alloc`**: Pure `core` only
//! - **Fixed-size TCB array**: `[TaskControlBlock; MAX_TASKS]`
//! - **Per-task stack**: `[u8; STACK_SIZE]` inline in TCB
//! - **Critical sections**: `cortex_m::interrupt::free()` for shared state

#![no_std]

pub mod config;
pub mod task;
pub mod game;
pub mod scheduler;
pub mod arch;
pub mod kernel;
pub mod sync;
