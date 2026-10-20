//! Fuzz `codec::decode_checked` (plan T-018).
//!
//! The property under test is narrow on purpose: **the decoder must return, not
//! panic and not allocate without bound**, for any byte string at all. It is not
//! "the decoder accepts only valid messages" — that is what the unit tests and
//! `sizes_match_spec` are for. A fuzzer is good at finding the input that makes
//! a `[u8; N]` deserialiser read one byte too far; it is bad at knowing what the
//! game should do.
//!
//! Run it with:
//!
//! ```text
//! cargo +nightly fuzz run decode -- -max_total_time=60
//! ```
//!
//! Every input that reaches the harness is capped at `MAX_FRAME_BYTES` by the
//! codec itself, so the interesting failures are in the payload decoders rather
//! than in the length check. To fuzz past the cap, `-max_len=5000` is enough:
//! libFuzzer grows inputs from the corpus, and an oversize frame is rejected
//! before it is parsed.

#![no_main]

use libfuzzer_sys::fuzz_target;
use protocol::codec::{decode_checked, Bounds};

fuzz_target!(|data: &[u8]| {
    // The only assertion is that this returns. A panic aborts the process and
    // libFuzzer reports the input; an allocation large enough to OOM does the
    // same, which is why the decoder never trusts a length field.
    let _ = decode_checked(data, Bounds::STANDARD);
});
