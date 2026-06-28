//! alarm-clock — slice 0 bootstrap.
//!
//! Only the bootstrap-config layer (tasks 1.3/1.4) is wired here. The full
//! runtime (tokio worker, axum, mopidy, slint) is layered on by later groups.

mod channel;
mod config;

use crate::config::Config;

fn main() {
    let cfg = Config::load();
    println!(
        "alarm-clock bootstrap: db_path={} mopidy_ws_url={} axum_bind_addr={} log_level={} data_dir={}",
        cfg.db_path, cfg.mopidy_ws_url, cfg.axum_bind_addr, cfg.log_level, cfg.data_dir
    );
}
