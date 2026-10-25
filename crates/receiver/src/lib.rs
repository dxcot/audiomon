//! Android receiver engine, loaded by the Kotlin app as `libaudiomon.so`.

#[cfg(target_os = "android")]
pub mod aaudio;
#[cfg(target_os = "android")]
pub mod engine;
#[cfg(target_os = "android")]
mod jni;
#[cfg(target_os = "android")]
mod logger;

pub use audiomon_core::PROTO_VERSION;
