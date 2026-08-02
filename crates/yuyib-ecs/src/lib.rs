//! ECS facade for Yuyib.
//!
//! The backend is deliberately isolated in this crate: public Yuyib modules
//! depend on this facade instead of scattering backend-specific imports.

#![forbid(unsafe_code)]

pub use bevy_ecs;

/// Common ECS imports for applications built with Yuyib.
pub mod prelude {
    pub use bevy_ecs::prelude::*;
}
