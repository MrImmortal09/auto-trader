pub mod portfolio;
pub mod settings;
pub mod auth_kotak;
pub use auth_kotak::{KotakLoginDeps, try_env_auto_login, auto_login_enabled_by_env};
pub mod auth_telegram;
pub mod auth_passkey;
pub mod health;

pub mod positions;

pub use portfolio::{webhook_handler, sse_logs_handler, portfolio_handler, logs_history_handler};
pub use positions::{positions_handler, delete_position_handler, patch_position_handler, close_position_handler, sell_position_handler, reconcile_preview_handler, reconcile_apply_handler, prices_handler, scrip_search_handler, scrip_download_handler};
pub use settings::{get_settings_handler, post_settings_handler, get_wallet_balance_handler, post_wallet_balance_handler, post_clear_database_handler, post_update_server_handler};
pub use auth_kotak::{kotak_login_handler, kotak_auto_login_handler, kotak_status_handler, kotak_scrip_raw_handler, kotak_scrip_json_handler, reset_creds, system_status, disconnect_kotak};
pub use auth_telegram::{
    telegram_request_code_handler, telegram_submit_code_handler,
    telegram_submit_2fa_handler, telegram_status_handler,
    telegram_chats_handler, telegram_start_handler, disconnect_telegram,
    resume_saved_telegram,
};
pub use health::{health_handler, version_handler};
pub use auth_passkey::{verify_passkey_handler, session_status_handler};
pub mod auth_phone;
pub use auth_phone::{otp_request_handler, otp_verify_handler, me_handler};
pub mod admin;
pub use admin::{list_users_handler, create_user_handler, patch_user_handler, delete_user_handler};
pub mod market;
pub use market::{market_indicator_handler, market_live_handler, market_option_chain_handler, market_trend_handler};
pub mod kill_switch;
pub use kill_switch::{post_kill_switch_handler, post_kill_switch_reset_handler, get_kill_switch_status_handler};
pub mod telemetry;
pub use telemetry::{metrics_handler, telemetry_snapshot_handler, telemetry_stream_handler};
pub mod execute;
pub use execute::{execute_stream_handler, execute_buy_handler, execute_exit_all_handler};
pub mod v2;
