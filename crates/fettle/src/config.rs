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

pub use error::{Error, Location};
use error::{SourceMap, locate};
use script::FETTLE_MODULE;

const PRELUDE: &str = include_str!("config/prelude.js");
const STATE_GLOBAL: &str = "__fettle";

/// A loaded `fettle.ts`: its registered hosts and the runtime that builds their catalogs.
///
/// Not `Send`: build catalogs on the thread that loaded the config.
pub struct Config {
    // Invariant: `hosts[i].index == i`, and `callbacks[i]` and `host_stacks[i]` belong to
    // `hosts[i]`.
    hosts: Vec<Host>,
    callbacks: Vec<Persistent<Function<'static>>>,
    host_stacks: Vec<String>,
    state: Persistent<Object<'static>>,
    file: PathBuf,
    module: String,
    map: SourceMap,
    context: Context,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    /// SSH destination, the host name unless the `ssh` option replaces it.
    pub ssh: String,
    pub tags: Vec<String>,
    index: usize,
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
        Error::new(self.location(stack), message)
    }

    fn caught(&self, caught: CaughtError<'_>) -> Error {
        match caught {
            CaughtError::Exception(exception) => self.error(
                &exception.stack().unwrap_or_default(),
                exception.message().unwrap_or_default(),
            ),
            CaughtError::Value(value) => Error::new(None, format!("uncaught {}", describe(&value))),
            CaughtError::Error(error) => Error::new(None, error.to_string()),
        }
    }
}

impl Config {
    /// Reads, strips and evaluates `fettle.ts` at `path`, registering its hosts.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read, has a syntax error, imports a module other than
    /// `"fettle"`, throws at the top level, never finishes, or registers an invalid host.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let source = std::fs::read_to_string(path)
            .map_err(|e| Error::new(None, format!("cannot read {}: {e}", path.display())))?;
        let script = script::strip(path, &source)?;
        let module = std::fs::canonicalize(path)
            .map_err(|e| Error::new(None, format!("cannot resolve {}: {e}", path.display())))?
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

        let (state, registered) = context.with(|ctx| -> Result<_, Error> {
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
                        None,
                        format!(
                            "{}: evaluation never finishes: a top-level await never settles",
                            path.display()
                        ),
                    ));
                }
                Err(caught) => return Err(js.caught(caught)),
            }
            let registered = register_hosts(&ctx, &js, &state)?;
            Ok((Persistent::save(&ctx, state), registered))
        })?;

        let (hosts, callbacks, host_stacks) = registered;
        Ok(Self {
            hosts,
            callbacks,
            host_stacks,
            state,
            file: path.to_path_buf(),
            module,
            map: script.map,
            context,
        })
    }

    /// Registered hosts, in registration order.
    #[must_use]
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    /// Runs the callback of `host`, one of [`Config::hosts`], with its facts.
    ///
    /// # Errors
    ///
    /// Fails when `host` is not one of [`Config::hosts`], or the callback throws, returns a
    /// thenable, declares an invalid resource or declares two different resources with the same
    /// identity.
    pub fn catalog(&self, host: &Host, facts: &Facts) -> Result<Catalog, Error> {
        if self
            .hosts
            .get(host.index)
            .is_none_or(|registered| registered.name != host.name)
        {
            return Err(Error::new(
                None,
                format!("host \"{}\" is not registered by this config", host.name),
            ));
        }
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
                let returned = self.run_callback(&ctx, &js, host, facts);
                state
                    .set("current", Value::new_null(ctx.clone()))
                    .map_err(engine_error)?;
                if is_thenable(&returned?) {
                    return Err(Error::new(
                        js.location(&self.host_stacks[host.index]),
                        "the callback must be synchronous, but it returned a promise",
                    ));
                }
                build_catalog(&ctx, &js, &declared)
            })
            .map_err(|e| Error {
                message: format!("host \"{}\": {}", host.name, e.message),
                ..e
            })
    }

    fn run_callback<'js>(
        &self,
        ctx: &Ctx<'js>,
        js: &Js<'_>,
        host: &Host,
        facts: &Facts,
    ) -> Result<Value<'js>, Error> {
        let callback = self.callbacks[host.index]
            .clone()
            .restore(ctx)
            .map_err(engine_error)?;
        let facts = serde_json::to_string(facts).map_err(|e| Error::new(None, e.to_string()))?;
        let facts = ctx.json_parse(facts).map_err(engine_error)?;
        callback
            .call::<_, Value>((facts,))
            .catch(ctx)
            .map_err(|e| js.caught(e))
    }
}

type Registered = (Vec<Host>, Vec<Persistent<Function<'static>>>, Vec<String>);

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
) -> Result<Registered, Error> {
    let entries: Vec<Array<'js>> = state.get("hosts").map_err(engine_error)?;
    let (mut hosts, mut callbacks, mut stacks) = (Vec::new(), Vec::new(), Vec::<String>::new());
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
        let options: HostOptions = from_js(ctx, entry.get(1).map_err(engine_error)?)
            .map_err(|e| js.error(&stack, format!("host \"{name}\": {}", field_error(&e))))?;
        if let Some(first) = hosts.iter().position(|h: &Host| h.name == name) {
            return Err(js.error(
                &stack,
                format!(
                    "host \"{name}\" is declared twice, first at {}",
                    describe_location(js, &stacks[first])
                ),
            ));
        }
        hosts.push(Host {
            ssh: options.ssh.unwrap_or_else(|| name.clone()),
            tags: options.tags.unwrap_or_default(),
            name,
            index: callbacks.len(),
        });
        callbacks.push(Persistent::save(ctx, callback));
        stacks.push(stack);
    }
    Ok((hosts, callbacks, stacks))
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
        let ids = ids(&entry.get(1).map_err(engine_error)?).ok_or_else(|| {
            js.error(
                &stack,
                format!("{kind}: the id must be a non-empty string or a list of them"),
            )
        })?;
        let props = match entry.get::<Value>(2).map_err(engine_error)? {
            props if props.is_undefined() => serde_json::json!({}),
            props => to_json(ctx, props)?,
        };
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
fn ids(value: &Value<'_>) -> Option<Vec<String>> {
    match value.as_array() {
        Some(list) => list
            .iter::<Value>()
            .map(|id| id.ok().as_ref().and_then(id_string))
            .collect(),
        None => id_string(value).map(|id| vec![id]),
    }
}

fn id_string(value: &Value<'_>) -> Option<String> {
    value
        .as_string()?
        .to_string()
        .ok()
        .filter(|id| !id.is_empty())
}

fn from_js<'js, T: serde::de::DeserializeOwned>(
    ctx: &Ctx<'js>,
    value: Value<'js>,
) -> Result<T, serde_path_to_error::Error<serde_json::Error>> {
    let json = if value.is_undefined() {
        serde_json::json!({})
    } else {
        to_json(ctx, value).unwrap_or(serde_json::Value::Null)
    };
    serde_path_to_error::deserialize(json)
}

fn to_json<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> Result<serde_json::Value, Error> {
    let Some(json) = ctx.json_stringify(value).map_err(engine_error)? else {
        return Ok(serde_json::Value::Null);
    };
    let json = json.to_string().map_err(engine_error)?;
    serde_json::from_str(&json).map_err(|e| Error::new(None, e.to_string()))
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

fn is_thenable(value: &Value<'_>) -> bool {
    value
        .as_object()
        .and_then(|object| object.get::<_, Value>("then").ok())
        .is_some_and(|then| then.is_function())
}

#[expect(clippy::needless_pass_by_value, reason = "passed to `map_err`")]
fn engine_error(error: rquickjs::Error) -> Error {
    Error::new(None, format!("JavaScript engine error: {error}"))
}
