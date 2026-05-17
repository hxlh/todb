/// Test utilities: tracing initializer.
///
/// Call `init_tracing!()` at the top of any test function:
/// ```rust
/// #[test]
/// fn test_something() {
///     init_tracing();
///     tracing::debug!("hello");
/// }
/// ```
#[cfg(test)]
pub fn init_tracing() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = tracing_subscriber::fmt()
            .with_file(true)
            .with_line_number(true)
            .with_target(false)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "debug".into()),
            )
            .with_test_writer()
            .try_init();
    });
}
