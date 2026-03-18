//! Application layer — egui UI, event loop, and thread orchestration.
//!
//! `SentruxApp` is the top-level eframe application. It owns all state,
//! spawns background threads (scanner, layout), and dispatches channel
//! messages in the update loop. Sub-modules handle individual UI panels.

pub mod breadcrumb;
pub mod canvas;
pub mod channels;
pub mod draw_panels;
pub mod mcp_server;
pub mod panels;
pub mod prefs;
pub mod progress;
pub mod scan_threads;
pub mod scanning;
#[cfg(test)]
mod scanning_tests;
pub mod settings_panel;
pub mod state;
pub mod status_bar;
pub mod update_check;
pub mod toolbar;
pub mod update_loop;
pub mod watcher;

use channels::{LayoutMsg, LayoutRequest, ScanCommand, ScanMsg};
use state::AppState;
use crate::core::snapshot::FileEvent;
use watcher::WatcherHandle;
use crossbeam_channel::{Receiver, Sender};

/// Main application — implements eframe::App.
/// Owns all mutable state and thread communication channels.
pub struct SentruxApp {
    /// All UI-visible mutable state
    pub(crate) state: AppState,
    /// Send commands to the scanner thread
    pub(crate) scan_tx: Sender<ScanCommand>,
    /// Receive results from the scanner thread
    pub(crate) scan_rx: Receiver<ScanMsg>,
    /// Send layout requests to the layout thread
    pub(crate) layout_tx: Sender<LayoutRequest>,
    /// Receive computed layout from the layout thread
    pub(crate) layout_rx: Receiver<LayoutMsg>,
    /// Receive filesystem events from the watcher
    pub(crate) watch_rx: Receiver<FileEvent>,
    /// Send end for watcher channel (kept to create new watchers)
    pub(crate) watch_tx: Sender<FileEvent>,
    /// Last root path that was successfully scanned
    pub(crate) last_scanned_root: Option<String>,
    /// Handle to the active filesystem watcher (drop stops watching)
    pub(crate) watcher_handle: Option<WatcherHandle>,
    /// Monotonic generation counter to reject stale scan results
    pub(crate) scan_generation: u64,
    /// Cancellation token for the current scan — set to true to abort
    pub(crate) scan_cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Receives watcher handle from background setup thread
    pub(crate) watcher_setup_rx: Option<crossbeam_channel::Receiver<Option<WatcherHandle>>>,
    /// Handle to the scanner thread (for dead-thread detection)
    pub(crate) scanner_handle: Option<std::thread::JoinHandle<()>>,
    /// Handle to the layout thread (for dead-thread detection)
    pub(crate) layout_handle: Option<std::thread::JoinHandle<()>>,
    /// Receives folder path from the background file picker dialog
    pub(crate) folder_picker_rx: Option<crossbeam_channel::Receiver<Option<String>>>,
    /// Platform pixels-per-point before any ui_scale is applied (captured at startup)
    pub(crate) native_ppp: f32,
}
