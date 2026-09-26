// integration test: rules.md §5's no-unwrap rule is scoped to non-test code;
// clippy's restriction lints don't auto-exempt files under tests/, so this is explicit.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use neuroos_ipc::{DEFAULT_MAX_FRAME, read_frame, write_frame};
use proptest::prelude::*;

proptest! {
    #[test]
    fn framing_roundtrips_arbitrary_payloads(payload in prop::collection::vec(any::<u8>(), 0..8192)) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (mut a, mut b) = tokio::io::duplex(payload.len() + 16);
            write_frame(&mut a, &payload, DEFAULT_MAX_FRAME).await.unwrap();
            let got = read_frame(&mut b, DEFAULT_MAX_FRAME).await.unwrap();
            prop_assert_eq!(got, Some(payload));
            Ok(())
        })?;
    }
}
