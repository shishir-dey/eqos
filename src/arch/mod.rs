//! # Architecture Abstraction Layer
//!
//! Provides a hardware abstraction boundary for the scheduler.
//! Currently implements the Cortex-M4 port; extensible to other
//! architectures by adding sibling modules.

pub mod cortex_m4;
