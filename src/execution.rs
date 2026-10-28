//! Per-request context on the local owner's serialized worker. Never change
//! process-wide environment or cwd while HTTP requests are running.
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Context {
    pub cwd: PathBuf,
    pub vars: HashMap<String, String>,
}

thread_local! {
    static CURRENT: RefCell<Option<Context>> = const { RefCell::new(None) };
    static OUTPUT: RefCell<Option<String>> = const { RefCell::new(None) };
}

impl Context {
    pub fn capture() -> std::io::Result<Self> {
        Ok(Self {
            cwd: std::env::current_dir()?,
            vars: supported_vars(std::env::vars_os())?,
        })
    }
}

fn supported_vars(
    variables: impl IntoIterator<Item = (OsString, OsString)>,
) -> std::io::Result<HashMap<String, String>> {
    let mut vars = HashMap::new();
    for (key, value) in variables {
        let Ok(key) = key.into_string() else { continue };
        if !key.starts_with("AI_WORKSPACE_")
            && !matches!(key.as_str(), "NO_COLOR" | "CLICOLOR" | "CLICOLOR_FORCE")
        {
            continue;
        }
        // Decode values only after discarding unrelated variables. Never put
        // an invalid value in the error: a selected variable may hold a token.
        let value = value.into_string().map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Environment variable {key} must contain valid Unicode"),
            )
        })?;
        vars.insert(key, value);
    }
    Ok(vars)
}

pub(crate) fn with<T>(context: &Context, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<Context>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(CURRENT.with(|slot| slot.replace(Some(context.clone()))));
    f()
}

pub(crate) fn cwd() -> std::io::Result<PathBuf> {
    CURRENT.with(|slot| match slot.borrow().as_ref() {
        Some(context) => Ok(context.cwd.clone()),
        None => std::env::current_dir(),
    })
}

pub(crate) fn var(name: &str) -> Result<String, std::env::VarError> {
    CURRENT.with(|slot| match slot.borrow().as_ref() {
        Some(context) => context
            .vars
            .get(name)
            .cloned()
            .ok_or(std::env::VarError::NotPresent),
        None => std::env::var(name),
    })
}

pub(crate) fn var_os(name: &str) -> Option<OsString> {
    CURRENT.with(|slot| match slot.borrow().as_ref() {
        Some(context) => context.vars.get(name).map(OsString::from),
        None => std::env::var_os(name),
    })
}

pub(crate) fn capture_output<T>(f: impl FnOnce() -> T) -> (T, String) {
    OUTPUT.with(|slot| *slot.borrow_mut() = Some(String::new()));
    let result = f();
    let output = OUTPUT.with(|slot| slot.borrow_mut().take().unwrap_or_default());
    (result, output)
}

pub(crate) fn println(args: std::fmt::Arguments<'_>) {
    OUTPUT.with(|slot| {
        if let Some(output) = slot.borrow_mut().as_mut() {
            use std::fmt::Write;
            writeln!(output, "{args}").expect("format CLI output");
        } else {
            std::println!("{args}");
        }
    });
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

    fn invalid_unicode() -> OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![0xff])
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0xd800])
        }
    }

    #[test]
    fn unrelated_non_unicode_environment_is_discarded_before_decoding_values() {
        let vars = supported_vars([
            (OsString::from("UNRELATED"), invalid_unicode()),
            (invalid_unicode(), invalid_unicode()),
            (
                OsString::from("AI_WORKSPACE_SCOPE"),
                OsString::from("current-project"),
            ),
        ])
        .unwrap();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars["AI_WORKSPACE_SCOPE"], "current-project");
    }

    #[test]
    fn selected_invalid_value_returns_an_error_without_exposing_its_contents() {
        let mut secret = OsString::from("do-not-log-this-secret");
        secret.push(invalid_unicode());
        let error =
            supported_vars([(OsString::from("AI_WORKSPACE_CLOUD_TOKEN"), secret)]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("AI_WORKSPACE_CLOUD_TOKEN"));
        assert!(!error.to_string().contains("do-not-log-this-secret"));
    }
}
