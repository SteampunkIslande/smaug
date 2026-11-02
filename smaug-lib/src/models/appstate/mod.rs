//! AppState module - runtime configuration management.
//!
//! Public API:
//! - `AppState` - Main entry point for getting/setting config values
//! - `AppStateConfigKey` - Registry enum for all config keys
//! - `ConfigItem` - Trait for strongly-typed config values
//! - `ValueInput` - Enum for HTML input rendering (Combo, Date)
//! - `SigintBehavior`, `ActiveRoutine` - Concrete config types

mod appstate_impl;
mod appstate_macros;

// `#[macro_use]` brings `define_app_configs!` and its `config_item_generator!`
// helper into textual scope here, so the configuration registry can be
// declared in this file while the macro machinery lives in `appstate_types`.
// The declaration must come before the `define_app_configs!` invocation below.
#[macro_use]
mod appstate_types;

pub use appstate_impl::AppState;
pub use appstate_macros::{ComboOption, ConfigItem, ValueInput};

define_app_configs! {
    pub enum SigintBehavior {
        db_key: "on_sigint",
        description: "Actions à mener lorsque cette application reçoit SIGINT",
        default: StopJobs,
        variants: [StopJobs => "Arrêter les jobs", WaitForJobCompletion => "Attendre la fin des jobs"],
    },

    pub enum ActiveRoutine {
        db_key: "active_routine",
        description: "Permet l'exécution immédiate de tous les runs...",
        default: RoutineOn,
        variants: [RoutineOff => "Routine désactivée", RoutineOn => "Routine activée"],
    }
}
