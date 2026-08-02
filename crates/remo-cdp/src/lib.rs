//! Remo speaking real CDP: HTTP discovery + WebSocket + Chrome's own
//! JSON-RPC envelope shape, instead of Remo's historical length-prefixed
//! custom framing.
//!
//! This crate is standalone by design (see the rewrite plan) — it depends on
//! nothing from `remo-protocol`/`remo-transport`/`remo-sdk`. `remo-sdk`
//! depends on *this* crate, not the other way around. `examples/standalone.rs`
//! proves the whole stack end to end (discovery, WS upgrade, domain dispatch)
//! against a real Chrome DevTools frontend without needing the rest of the
//! workspace to exist yet.
//!
//! Domain implementations live in sibling modules and register with
//! [`Dispatcher`]:
//! - [`domain_remo`] — the custom `Remo.*` domain: `Remo.invoke`,
//!   `Remo.listCapabilities`, event `Remo.capabilitiesChanged`. This is Track
//!   A (raw wire compatibility) — the actual product.
//! - [`domain_page`] — `Page.captureScreenshot`/`Page.startScreencast` plus
//!   the bootstrap-safe stub table for `Runtime`/`Debugger`/`Log`/`Network`/
//!   `Emulation` — Track B (real Chrome DevTools frontend compatibility).
//! - [`domain_dom`] — `DOM`/`CSS`/`Overlay`: the UIView tree as an Elements
//!   panel.
//! - [`domain_storage`] — `DOMStorage`/`Storage`: `NSUserDefaults` as the
//!   Application panel's Local Storage table.

pub mod discovery;
pub mod dispatcher;
pub mod domain_dom;
pub mod domain_page;
pub mod domain_remo;
pub mod domain_storage;
pub mod dual_stack;
pub mod remote_object;
pub mod transport;

pub use dispatcher::{CdpDomain, CdpReply, CdpRequest, Dispatcher, EventSink};
