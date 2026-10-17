const state = globalThis.__fettle;
delete globalThis.__fettle;

function stack() {
  return new Error().stack;
}

export function host(name, a, b) {
  if (state.current !== null) {
    throw new Error("host() must be called at the top level of fettle.ts, not inside a host callback");
  }
  const [options, callback] = arguments.length < 3 ? [undefined, a] : [a, b];
  state.hosts.push([name, options, callback, stack()]);
}

function resource(kind) {
  return (id, props) => {
    if (state.current === null) {
      throw new Error(`${kind}() must be called inside a host callback`);
    }
    state.current.push([kind, id, props, stack()]);
  };
}

export const pkg = resource("pkg");
export const file = resource("file");
export const service = resource("service");
export const user = resource("user");

export function read(path) {
  const [text, error] = state.read(String(path));
  if (error !== undefined) {
    throw new Error(error);
  }
  return text;
}
