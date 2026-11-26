//! Session lifecycle, loading, picking, modal, and fork dispatchers.

pub(in crate::app::dispatch) mod foreign;
pub(in crate::app::dispatch) mod fork;
pub(in crate::app::dispatch) mod lifecycle;
pub(in crate::app::dispatch) mod load;
pub(in crate::app::dispatch) mod modal;
