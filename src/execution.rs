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
            vars: std::env::vars()
                .filter(|(key, _)| {
                    key.starts_with("AI_WORKSPACE_")
                        || matches!(key.as_str(), "NO_COLOR" | "CLICOLOR" | "CLICOLOR_FORCE")
                })
                .collect(),
        })
    }
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
