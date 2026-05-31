// Copyright (c) 2026, FreeOCD
// SPDX-License-Identifier: BSD-3-Clause

//! Functionality derived from the MIT-licensed dapjs project.
//!
//! This module isolates code ported from dapjs
//! (<https://github.com/ARMmbed/dapjs>). At present that is the SEGGER RTT
//! handling in [`rtt`], derived from dapjs's `examples/rtt/rtt.js`. The bundled
//! `LICENSE` file is that example's MIT license (Copyright (C) 2021 Ciro
//! Cattuto).

pub mod rtt;
