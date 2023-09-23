MIRI command

```bash
MIRIFLAGS="-Zmiri-permissive-provenance -Zmiri-disable-isolation -Zmiri-backtrace=full" cargo +nightly miri test -- --nocapture
```