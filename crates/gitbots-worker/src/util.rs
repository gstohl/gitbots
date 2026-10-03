//! Clock, randomness and ids from the Workers runtime.

use gitbots_core::Ulid;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = crypto, js_name = getRandomValues)]
    fn get_random_values(buf: &mut [u8]);
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    get_random_values(&mut buf);
    buf
}

/// Milliseconds since the Unix epoch. (In Workers the clock advances only
/// across IO, which is fine for ids and timestamps.)
pub fn now_ms() -> u64 {
    // Date.now() is a non-negative integer far below 2^53.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let ms = js_sys::Date::now() as u64;
    ms
}

/// RFC 3339, UTC, millisecond precision.
pub fn now_rfc3339() -> String {
    js_sys::Date::new_0().to_iso_string().into()
}

pub fn new_ulid() -> Ulid {
    let r = random_bytes::<16>();
    Ulid::from_parts(now_ms(), u128::from_le_bytes(r))
}
