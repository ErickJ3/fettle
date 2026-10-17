//! Evaluates `fettle.ts` into registered hosts and, given a host's facts, its catalog.

mod error;
mod script;

use std::path::{Path, PathBuf};

use fettle_protocol::{Catalog, Desired, Facts, Resource};
use rquickjs::convert::List;
use rquickjs::loader::{BuiltinLoader, BuiltinResolver};
use rquickjs::{
    Array, CatchResultExt, CaughtError, Context, Ctx, Function, Module, Object, Persistent,
    Runtime, Value,
};
use serde::Deserialize;

pub use error::{Error, ErrorKind, Location};
use error::{SourceMap, locate};
use script::FETTLE_MODULE;

const PRELUDE: &str = include_str!("config/prelude.js");
const STATE_GLOBAL: &str = "__fettle";

/// A loaded `fettle.ts`: its registered hosts and the runtime that builds their catalogs.
///
/// Not `Send`: build catalogs on the thread that loaded the config.
pub struct Config {
    // Invariant: `registrations[i].host.index == i`.
    registrations: Vec<Registration>,
    state: Persistent<Object<'static>>,
    file: PathBuf,
    module: String,
    map: SourceMap,
    context: Context,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    name: String,
    ssh: String,
    tags: Vec<String>,
    index: usize,
}

impl Host {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// SSH destination, the host name unless the `ssh` option replaces it.
    #[must_use]
    pub fn ssh(&self) -> &str {
        &self.ssh
    }

    #[must_use]
    pub fn tags(&self) -> &[String] {
        &self.tags
    }
}

/// A host with its callback and the stack of its declaration.
struct Registration {
    host: Host,
    callback: Persistent<Function<'static>>,
    stack: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostOptions {
    ssh: Option<String>,
    tags: Option<Vec<String>>,
}

struct Js<'a> {
    file: &'a Path,
    module: &'a str,
    map: &'a SourceMap,
}

impl Js<'_> {
    fn location(&self, stack: &str) -> Option<Location> {
        locate(stack, self.module, self.map, self.file)
    }

    fn error(&self, stack: &str, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Script, self.location(stack), message)
    }

    fn caught(&self, caught: CaughtError<'_>) -> Error {
        match caught {
            CaughtError::Exception(exception) => self.error(
                &exception.stack().unwrap_or_default(),
                exception.message().unwrap_or_default(),
            ),
            CaughtError::Value(value) => Error::new(
                ErrorKind::Script,
                None,
                format!("uncaught {}", describe(&value)),
            ),
            CaughtError::Error(error) => engine_error(error),
        }
    }

    /// Like [`Js::caught`], falling back to the declaration at `stack` for a throw that has no
    /// location of its own, such as a thrown string or one from `JSON.stringify` itself.
    fn caught_at(&self, caught: CaughtError<'_>, stack: &str) -> Error {
        let mut error = self.caught(caught);
        error.location = error.location.or_else(|| self.location(stack));
        error
    }
}

impl Config {
    /// Reads, strips and evaluates `fettle.ts` at `path`, registering its hosts.
    ///
    /// # Errors
    ///
    /// - [`ErrorKind::Io`]: the file cannot be read or resolved.
    /// - [`ErrorKind::Syntax`]: it is not valid TypeScript, imports a module other than
    ///   `"fettle"` or uses `import()`.
    /// - [`ErrorKind::Script`]: it throws at the top level, never finishes, or registers an
    ///   invalid host.
    /// - [`ErrorKind::Engine`]: the JavaScript engine fails.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let file = path.into();
        let path = file.as_path();
        let source = std::fs::read_to_string(path).map_err(|e| {
            Error::new(
                ErrorKind::Io,
                None,
                format!("cannot read {}", path.display()),
            )
            .caused_by(e)
        })?;
        let script = script::strip(path, &source)?;
        let module = std::fs::canonicalize(path)
            .map_err(|e| {
                Error::new(
                    ErrorKind::Io,
                    None,
                    format!("cannot resolve {}", path.display()),
                )
                .caused_by(e)
            })?
            .to_string_lossy()
            .into_owned();
        let base = path.parent().unwrap_or(Path::new("")).to_path_buf();

        let runtime = Runtime::new().map_err(engine_error)?;
        runtime.set_loader(
            BuiltinResolver::default().with_module(FETTLE_MODULE),
            BuiltinLoader::default().with_module(FETTLE_MODULE, PRELUDE),
        );
        let context = Context::full(&runtime).map_err(engine_error)?;
        let js = Js {
            file: path,
            module: &module,
            map: &script.map,
        };

        let (state, registrations) = context.with(|ctx| -> Result<_, Error> {
            let state = new_state(&ctx, base).map_err(engine_error)?;
            ctx.globals()
                .set(STATE_GLOBAL, state.clone())
                .map_err(engine_error)?;
            let evaluated = Module::evaluate(ctx.clone(), module.as_str(), script.code.as_str())
                .catch(&ctx)
                .map_err(|e| js.caught(e))?;
            match evaluated.finish::<()>().catch(&ctx) {
                Ok(()) => {}
                Err(CaughtError::Error(rquickjs::Error::WouldBlock)) => {
                    return Err(Error::new(
                        ErrorKind::Script,
                        None,
                        format!(
                            "{}: evaluation never finishes: a top-level await never settles",
                            path.display()
                        ),
                    ));
                }
                Err(caught) => return Err(js.caught(caught)),
            }
            let registrations = register_hosts(&ctx, &js, &state)?;
            Ok((Persistent::save(&ctx, state), registrations))
        })?;

        Ok(Self {
            registrations,
            state,
            file,
            module,
            map: script.map,
            context,
        })
    }

    /// Registered hosts, in registration order.
    pub fn hosts(&self) -> impl ExactSizeIterator<Item = &Host> {
        self.registrations
            .iter()
            .map(|registration| &registration.host)
    }

    /// Runs the callback of `host`, one of [`Config::hosts`], with its facts.
    ///
    /// # Errors
    ///
    /// - [`ErrorKind::UnknownHost`]: `host` is not one of [`Config::hosts`].
    /// - [`ErrorKind::Script`]: the callback throws, returns a thenable, declares an invalid
    ///   resource or declares two different resources with the same identity.
    /// - [`ErrorKind::Engine`]: the JavaScript engine fails.
    pub fn catalog(&self, host: &Host, facts: &Facts) -> Result<Catalog, Error> {
        let registration = self.registration(host)?;
        let js = Js {
            file: &self.file,
            module: &self.module,
            map: &self.map,
        };
        self.context
            .with(|ctx| {
                let state = self.state.clone().restore(&ctx).map_err(engine_error)?;
                let declared = Array::new(ctx.clone()).map_err(engine_error)?;
                state
                    .set("current", declared.clone())
                    .map_err(engine_error)?;
                let returned = run_callback(&ctx, &js, registration, facts);
                state
                    .set("current", Value::new_null(ctx.clone()))
                    .map_err(engine_error)?;
                let thenable = is_thenable(&returned?)
                    .catch(&ctx)
                    .map_err(|e| js.caught_at(e, &registration.stack))?;
                if thenable {
                    return Err(Error::new(
                        ErrorKind::Script,
                        js.location(&registration.stack),
                        "the callback must be synchronous, but it returned a promise",
                    ));
                }
                build_catalog(&ctx, &js, &declared)
            })
            .map_err(|e| e.prefixed(&format!("host \"{}\"", host.name)))
    }

    fn registration(&self, host: &Host) -> Result<&Registration, Error> {
        self.registrations
            .get(host.index)
            .filter(|registration| registration.host == *host)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::UnknownHost,
                    None,
                    format!("host \"{}\" is not registered by this config", host.name),
                )
            })
    }
}

fn run_callback<'js>(
    ctx: &Ctx<'js>,
    js: &Js<'_>,
    registration: &Registration,
    facts: &Facts,
) -> Result<Value<'js>, Error> {
    let callback = registration
        .callback
        .clone()
        .restore(ctx)
        .map_err(engine_error)?;
    let facts = serde_json::to_string(facts).map_err(|e| {
        Error::new(ErrorKind::Engine, None, "cannot convert the facts to JSON").caused_by(e)
    })?;
    let facts = ctx.json_parse(facts).map_err(engine_error)?;
    callback
        .call::<_, Value>((facts,))
        .catch(ctx)
        .map_err(|e| js.caught_at(e, &registration.stack))
}

fn new_state<'js>(ctx: &Ctx<'js>, base: PathBuf) -> rquickjs::Result<Object<'js>> {
    let state = Object::new(ctx.clone())?;
    state.set("hosts", Array::new(ctx.clone())?)?;
    state.set("current", Value::new_null(ctx.clone()))?;
    let read = move |path: String| match read_local(&base, &path) {
        Ok(text) => List((Some(text), None)),
        Err(message) => List((None, Some(message))),
    };
    state.set("read", Function::new(ctx.clone(), read)?)?;
    Ok(state)
}

/// Resolves `path` as `read()` does: `~` against the operator's home, relative against `base`.
fn read_local(base: &Path, path: &str) -> Result<String, String> {
    let resolved = match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => std::env::home_dir()
            .ok_or_else(|| format!("cannot resolve {path}: no home directory"))?
            .join(rest.trim_start_matches('/')),
        _ => base.join(path),
    };
    std::fs::read_to_string(&resolved)
        .map_err(|e| format!("cannot read {}: {e}", resolved.display()))
}

fn register_hosts<'js>(
    ctx: &Ctx<'js>,
    js: &Js<'_>,
    state: &Object<'js>,
) -> Result<Vec<Registration>, Error> {
    let entries: Vec<Array<'js>> = state.get("hosts").map_err(engine_error)?;
    let mut registrations: Vec<Registration> = Vec::new();
    for entry in entries {
        let stack: String = entry.get(3).map_err(engine_error)?;
        let name = entry
            .get::<Value>(0)
            .map_err(engine_error)?
            .as_string()
            .map(rquickjs::String::to_string)
            .transpose()
            .map_err(engine_error)?
            .filter(|name| !name.is_empty())
            .ok_or_else(|| js.error(&stack, "host name must be a non-empty string"))?;
        let callback = entry
            .get::<Value>(2)
            .map_err(engine_error)?
            .into_function()
            .ok_or_else(|| js.error(&stack, format!("host \"{name}\" needs a callback")))?;
        let options = to_json(ctx, js, &stack, entry.get(1).map_err(engine_error)?)
            .map_err(|e| e.prefixed(&format!("host \"{name}\"")))?;
        let options: HostOptions = serde_path_to_error::deserialize(options)
            .map_err(|e| js.error(&stack, format!("host \"{name}\": {}", field_error(&e))))?;
        if let Some(first) = registrations.iter().find(|r| r.host.name == name) {
            return Err(js.error(
                &stack,
                format!(
                    "host \"{name}\" is declared twice, first at {}",
                    describe_location(js, &first.stack)
                ),
            ));
        }
        registrations.push(Registration {
            host: Host {
                ssh: options.ssh.unwrap_or_else(|| name.clone()),
                tags: options.tags.unwrap_or_default(),
                name,
                index: registrations.len(),
            },
            callback: Persistent::save(ctx, callback),
            stack,
        });
    }
    Ok(registrations)
}

fn build_catalog<'js>(
    ctx: &Ctx<'js>,
    js: &Js<'_>,
    declared: &Array<'js>,
) -> Result<Catalog, Error> {
    let mut catalog = Catalog::new();
    let mut stacks: Vec<String> = Vec::new();
    for entry in declared.iter::<Array>() {
        let entry = entry.map_err(engine_error)?;
        let kind: String = entry.get(0).map_err(engine_error)?;
        let stack: String = entry.get(3).map_err(engine_error)?;
        let ids = ids(&entry.get(1).map_err(engine_error)?)
            .catch(ctx)
            .map_err(|e| js.caught_at(e, &stack))?
            .ok_or_else(|| {
                js.error(
                    &stack,
                    format!("{kind}: the id must be a non-empty string or a list of them"),
                )
            })?;
        let props = to_json(ctx, js, &stack, entry.get(2).map_err(engine_error)?)
            .map_err(|e| e.prefixed(&kind))?;
        for id in ids {
            let desired: Desired = serde_path_to_error::deserialize(
                serde_json::json!({ "kind": kind, "props": props }),
            )
            .map_err(|e| js.error(&stack, format!("{kind} {id}: {}", field_error(&e))))?;
            let resource = Resource { id, desired };
            match catalog.iter().position(|r| r.key() == resource.key()) {
                Some(first) if catalog[first] == resource => {}
                Some(first) => {
                    return Err(js.error(
                        &stack,
                        format!(
                            "{kind} {} conflicts with the declaration at {}",
                            resource.id,
                            describe_location(js, &stacks[first])
                        ),
                    ));
                }
                None => {
                    catalog.push(resource);
                    stacks.push(stack.clone());
                }
            }
        }
    }
    Ok(catalog)
}

/// A non-empty string is one id and a list of them is each; anything else is `None`.
fn ids(value: &Value<'_>) -> rquickjs::Result<Option<Vec<String>>> {
    match value.as_array() {
        Some(list) => list.iter::<Value>().map(|id| id_string(&id?)).collect(),
        None => Ok(id_string(value)?.map(|id| vec![id])),
    }
}

fn id_string(value: &Value<'_>) -> rquickjs::Result<Option<String>> {
    let id = value
        .as_string()
        .map(rquickjs::String::to_string)
        .transpose()?;
    Ok(id.filter(|id| !id.is_empty()))
}

/// Converts the options or props of the declaration at `stack`, `undefined` meaning none.
fn to_json<'js>(
    ctx: &Ctx<'js>,
    js: &Js<'_>,
    stack: &str,
    value: Value<'js>,
) -> Result<serde_json::Value, Error> {
    if value.is_undefined() {
        return Ok(serde_json::json!({}));
    }
    let json = match ctx.json_stringify(value).catch(ctx) {
        Ok(Some(json)) => json.to_string().map_err(engine_error)?,
        Ok(None) => return Ok(serde_json::Value::Null),
        Err(caught) => return Err(js.caught_at(caught, stack)),
    };
    serde_json::from_str(&json).map_err(|e| {
        Error::new(ErrorKind::Engine, None, "the engine produced invalid JSON").caused_by(e)
    })
}

/// Names the failing field without the `props.` prefix of a resource declaration.
fn field_error(error: &serde_path_to_error::Error<serde_json::Error>) -> String {
    let path = error.path().to_string();
    let inner = error.inner().to_string();
    match path.strip_prefix("props.").unwrap_or(&path) {
        "." | "props" => inner,
        field if inner.contains(&format!("`{field}`")) => inner,
        field => format!("`{field}`: {inner}"),
    }
}

fn describe_location(js: &Js<'_>, stack: &str) -> String {
    js.location(stack)
        .map_or_else(|| "an unknown location".to_owned(), |l| l.to_string())
}

fn describe(value: &Value<'_>) -> String {
    value
        .as_string()
        .and_then(|s| s.to_string().ok())
        .map_or_else(|| format!("{:?}", value.type_of()), |s| format!("\"{s}\""))
}

fn is_thenable(value: &Value<'_>) -> rquickjs::Result<bool> {
    match value.as_object() {
        Some(object) => Ok(object.get::<_, Value>("then")?.is_function()),
        None => Ok(false),
    }
}

fn engine_error(error: rquickjs::Error) -> Error {
    Error::new(ErrorKind::Engine, None, "JavaScript engine error").caused_by(error)
}
